(function () {
    let stateWasNull = false;

    const state = {
        allowed: [],
        centre: 'All',
        roles: 0n,
        list: null,
        lang: null,
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
                state[key] = saved[key];
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
    }

    function reflectState() {
        let category = document.getElementById('category-filter');
        for (let option of category.options) {
            if (stateWasNull) {
                console.log('was null');
                state.allowed.push(option.value);
            }

            option.selected = state.allowed.includes(option.value);
        }

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
            // In case any unnecessary bits were set
            state.roles = newRolesState;
        }

        let language = document.getElementById('language');
        if (state.lang === null) {
            state.lang = language.dataset.accept;
        }

        let cookie = document.cookie
            .split(';')
            .find(row => row.trim().startsWith('lang='));
        if (cookie !== undefined) {
            state.lang = decodeURIComponent(cookie.split('=')[1]);
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
        function categoryFilter(item) {
            let category = item.elm.dataset.pfCategory;

            return category === 'unknown' || state.allowed.includes(category);
        }

        function dataCentreFilter(item) {
            return state.centre === "All" || state.centre === item.values().centre;
        }

        function roleFilter(item) {
            // Do not filter alliance raid / pvp because the jobs present are not accurate
            if (Number(item.elm.dataset.numParties) !== 1) {
                return true;
            }
            return state.roles === 0n || state.roles & BigInt(item.elm.dataset.joinableRoles);
        }

        state.list.filter(item => dataCentreFilter(item) && categoryFilter(item) && roleFilter(item));
    }

    function setUpDataCentreFilter() {
        let select = document.getElementById('data-centre-filter');

        let dataCentres = {};
        for (let elem of document.querySelectorAll('#listings > .listing')) {
            let centre = elem.dataset['centre'];
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

    function setUpCategoryFilter() {
        let select = document.getElementById('category-filter');

        select.addEventListener('change', () => {
            let allowed = [];

            for (let option of select.options) {
                if (!option.selected) {
                    continue;
                }

                let category = option.value;
                allowed.push(category);
            }

            state.allowed = allowed;
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
    reflectState();
    state.list = setUpList();
    setUpDataCentreFilter();
    setUpCategoryFilter();
    setUpRoleFilter();
    refilter();
    setupPaginationNav();
})();
