(function () {
    let stateWasNull = false;

    const state = {
        allowed: [],
        centre: 'All',
        roles: 0n,
        list: null,
        lang: null,
        highEnd: true, // Default to true
        objectives: 0,
        conditions: 0,
        onePlayerPerJob: false,
        minItemLevel: 0,
    };

    function addJsClass() {
        document.children[0].className = 'js';
    }

    function saveLoadState() {
        let saved = localStorage.getItem('state');
        if (saved !== null) {
            try {
                saved = JSON.parse(saved, (key, value) => key === 'roles' ?
                    BigInt(value) : value);
                if (!Array.isArray(saved.allowed)) {
                    saved = {};
                    stateWasNull = true;
                }
            } catch (e) {
                saved = {};
                stateWasNull = true;
            }

            for (let key in saved) {
                if (state.hasOwnProperty(key)) {
                    state[key] = saved[key];
                }
            }
        } else {
            stateWasNull = true;
        }

        window.addEventListener('pagehide', () => {
            let copy = {};
            for (let key in state) {
                if (key === 'list') {
                    continue;
                }

                copy[key] = state[key];
            }

            localStorage.setItem('state', JSON.stringify(copy, (_, value) =>
                typeof value === 'bigint' ? value.toString() : value));
        });

        // Min Item Level
        const minILFilter = document.getElementById('min-il-filter');
        if (minILFilter) {
            minILFilter.addEventListener('input', (e) => {
                let val = parseInt(e.target.value);
                if (isNaN(val) || val < 0) val = 0;
                state.minItemLevel = val;
                refilter();
            });
        }
    }

    function reflectState() {
        // category-filter was removed from the template
        // let category = document.getElementById('category-filter');
        // for (let option of category.options) {
        //     if (stateWasNull) {
        //         console.log('was null');
        //         state.allowed.push(option.value);
        //     }
        //     option.selected = state.allowed.includes(option.value);
        // }

        let dataCentre = document.getElementById('data-centre-filter');
        dataCentre.value = state.centre;

        if (stateWasNull || state.roles <= 0n) {
            state.roles = 0n;
        } else {
            let roleFilterInputs = document.getElementById('role-filter')
                .getElementsByTagName('input');
            let newRolesState = 0n;
            for (let input of roleFilterInputs) {
                let value = BigInt(input.value);
                if (state.roles & value) {
                    input.checked = true;
                    newRolesState |= value;
                }
            }
            state.roles = newRolesState;
        }

        // New filters reflect
        document.getElementById('high-end-filter').checked = state.highEnd;

        const objectiveInputs = document.getElementById('objective-filter').getElementsByTagName('input');
        for (let input of objectiveInputs) {
            let val = parseInt(input.value);
            input.checked = (state.objectives & val) !== 0;
        }

        const conditionInputs = document.getElementById('condition-filter').getElementsByTagName('input');
        for (let input of conditionInputs) {
            let val = parseInt(input.value);
            if (val === 32) { // One Player per Job
                input.checked = state.onePlayerPerJob;
            } else {
                input.checked = (state.conditions & val) !== 0;
            }
        }



        const minILInput = document.getElementById('min-il-filter');
        if (minILInput) {
            minILInput.value = state.minItemLevel || 0;
        }

        // Language priority: cookie > dataset.accept > localStorage
        let language = document.getElementById('language');
        let cookie = document.cookie
            .split(';')
            .find(row => row.trim().startsWith('lang='));

        if (cookie !== undefined) {
            state.lang = decodeURIComponent(cookie.split('=')[1]);
        } else if (language && language.dataset.accept) {
            state.lang = language.dataset.accept;
        } else if (state.lang === null) {
            state.lang = 'en'; // fallback
        }
    }

    function setUpList() {
        let options = {
            valueNames: [
                'duty',
                'creator',
                'description',
                { data: ['centre'] },
            ],
            page: 50,
            pagination: {
                innerWindow: 2,
                outerWindow: 1,
                paginationClass: 'pagination',
            },
        };
        return new List('container', options);
    }

    function refilter() {
        function dataCentreFilter(item) {
            return state.centre === "All" || state.centre === item.values().centre;
        }

        function roleFilter(item) {
            if (Number(item.elm.dataset.numParties) !== 1) {
                return true;
            }
            return state.roles === 0n || state.roles & BigInt(item.elm.dataset.joinableRoles);
        }

        function highEndFilter(item) {
            if (!state.highEnd) return true;
            return item.elm.dataset.highEnd === 'true';
        }

        function objectiveFilter(item) {
            if (state.objectives === 0) return true;
            let itemObj = parseInt(item.elm.dataset.objective || '0');
            // OR logic: show if listing matches ANY of the selected objectives
            // But usually for objectives like 'Practice', 'Loot', listings only have one main flag locally,
            // but server sends bitflags.
            // If listing has 'Practice' and I select 'Practice', (itemObj & state.objectives) will be non-zero.
            return (itemObj & state.objectives) !== 0;
        }

        function conditionFilter(item) {
            // Conditions (Duty Complete, etc)
            if (state.conditions !== 0) {
                let itemCond = parseInt(item.elm.dataset.conditions || '0');
                if ((itemCond & state.conditions) === 0) return false;
            }

            // One Player Per Job (Search Area flag 32)
            if (state.onePlayerPerJob) {
                let itemSearchArea = parseInt(item.elm.dataset.searchArea || '0');
                // 32 = 1<<5
                if ((itemSearchArea & 32) === 0) return false;
            }

            return true;
        }

        function minItemLevelFilter(item) {
            if (!state.minItemLevel || state.minItemLevel <= 0) return true;
            let itemLevel = parseInt(item.elm.dataset.minItemLevel || '0');
            return itemLevel >= state.minItemLevel;
        }

        state.list.filter(item =>
            dataCentreFilter(item) &&
            roleFilter(item) &&
            highEndFilter(item) &&
            objectiveFilter(item) &&
            objectiveFilter(item) &&
            conditionFilter(item) &&
            minItemLevelFilter(item)
        );
    }

    function setUpDataCentreFilter() {
        let select = document.getElementById('data-centre-filter');

        let dataCentres = {};
        for (let item of state.list.items) {
            let centre = item.values().centre;
            if (!dataCentres.hasOwnProperty(centre)) {
                dataCentres[centre] = 0;
            }

            dataCentres[centre] += 1;
        }

        for (let opt of select.options) {
            let centre = opt.value;

            let count = 0;

            if (dataCentres.hasOwnProperty(centre)) {
                count = dataCentres[centre];
            }

            if (centre === 'All') {
                count = Object.values(dataCentres).reduce((a, b) => a + b, 0);
            }

            opt.innerText += ` (${count})`;
        }

        select.addEventListener('change', () => {
            state.centre = select.value;
            refilter();
        });
    }



    function setUpRoleFilter() {
        let select = document.getElementById('role-filter');

        select.addEventListener('change', (event) => {
            let value = BigInt(event.target.value);
            if (event.target.checked) {
                state.roles |= value;
            } else {
                state.roles &= ~value;
            }
            refilter();
        });
    }

    function setUpAdvancedFilters() {
        // High-end
        const highEnd = document.getElementById('high-end-filter');
        highEnd.addEventListener('change', (e) => {
            state.highEnd = e.target.checked;
            refilter();
        });

        // Objective
        const objFilter = document.getElementById('objective-filter');
        objFilter.addEventListener('change', (e) => {
            const val = parseInt(e.target.value);
            if (e.target.checked) {
                state.objectives |= val;
            } else {
                state.objectives &= ~val;
            }
            refilter();
        });

        // Condition
        const condFilter = document.getElementById('condition-filter');
        condFilter.addEventListener('change', (e) => {
            const val = parseInt(e.target.value);
            if (val === 32) {
                state.onePlayerPerJob = e.target.checked;
            } else {
                if (e.target.checked) {
                    state.conditions |= val;
                } else {
                    state.conditions &= ~val;
                }
            }
            refilter();
        });
    }

    function setupPaginationNav() {
        let prev = document.querySelector('.page-btn.prev');
        let next = document.querySelector('.page-btn.next');

        if (!prev || !next) return;

        function updateButtons() {
            let list = state.list;
            let i = list.i || 1;
            let page = list.page;

            // 첫 페이지면 prev 비활성화
            if (i <= 1) {
                prev.classList.add('disabled');
            } else {
                prev.classList.remove('disabled');
            }

            // 마지막 페이지면 next 비활성화
            if (i + page > list.size()) {
                next.classList.add('disabled');
            } else {
                next.classList.remove('disabled');
            }

            // '...' 항목 비활성화 (List.js가 렌더링한 후)
            setTimeout(() => {
                let paginationLinks = document.querySelectorAll('.pagination li a');
                paginationLinks.forEach(a => {
                    if (a.innerText === '...') {
                        a.parentElement.classList.add('disabled');
                    }
                });
            }, 0);
        }

        prev.addEventListener('click', (e) => {
            e.preventDefault();
            let list = state.list;
            let i = (list.i || 1) - list.page;
            if (i < 1) i = 1;
            list.show(i, list.page);
        });

        next.addEventListener('click', (e) => {
            e.preventDefault();
            let list = state.list;
            let i = (list.i || 1) + list.page;
            if (i > list.size()) return;
            list.show(i, list.page);
        });

        state.list.on('updated', updateButtons);
        updateButtons();
    }

    addJsClass();
    saveLoadState();
    // Translations handled by translations.js


    function applyTranslations() {
        const lang = state.lang || 'en';
        document.querySelectorAll('[data-i18n]').forEach(elem => {
            const key = elem.dataset.i18n;
            if (TRANSLATIONS[key] && TRANSLATIONS[key][lang]) {
                elem.textContent = TRANSLATIONS[key][lang];
            }
        });
    }

    reflectState();
    state.list = setUpList();
    setUpDataCentreFilter();
    setUpRoleFilter();
    setUpAdvancedFilters();
    applyTranslations(); // Apply translations on load
    refilter();
    setupPaginationNav();
})();
