use std::{cmp::Ordering, collections::HashMap, convert::Infallible, sync::Arc, time::Duration};
use futures_util::stream::StreamExt;

use anyhow::{Context, Result};
use mongodb::{
    options::IndexOptions
    ,
    Client as MongoClient, Collection, IndexModel,
};
use tokio::sync::broadcast::Sender;
use tokio::sync::RwLock;
use warp::{filters::BoxedFilter, http::Uri, Filter, Reply};

use crate::api::api;
use crate::mongo::{get_current_listings, insert_listing, upsert_players, get_players_by_content_ids};
use crate::player::{Player, UploadablePlayer};
use crate::{
    config::Config, ffxiv::Language, listing::PartyFinderListing,
    listing_container::ListingContainer, stats::CachedStatistics,
    template::listings::ListingsTemplate, template::stats::StatsTemplate,
};

mod stats;

pub async fn start(config: Arc<Config>) -> Result<()> {
    let state = State::new(Arc::clone(&config)).await?;

    println!("listening at {}", config.web.host);
    warp::serve(router(state)).run(config.web.host).await;
    Ok(())
}

pub struct State {
    pub mongo: MongoClient,
    pub stats: RwLock<Option<CachedStatistics>>,
    pub listings_channel: Sender<Arc<[PartyFinderListing]>>,
    pub fflogs_client: Option<crate::fflogs::FFLogsClient>,
}

impl State {
    pub async fn new(config: Arc<Config>) -> Result<Arc<Self>> {
        let mongo = MongoClient::with_uri_str(&config.mongo.url)
            .await
            .context("could not create mongodb client")?;
            
        let fflogs_client = config.fflogs.clone().map(crate::fflogs::FFLogsClient::new);

        let (tx, _) = tokio::sync::broadcast::channel(16);
        let state = Arc::new(Self {
            mongo,
            stats: Default::default(),
            listings_channel: tx,
            fflogs_client,
        });

        state
            .collection()
            .create_index(
                IndexModel::builder()
                    .keys(mongodb::bson::doc! {
                        "listing.id": 1,
                        "listing.last_server_restart": 1,
                        "listing.created_world": 1,
                    })
                    .options(IndexOptions::builder().unique(true).build())
                    .build(),
                None,
            )
            .await
            .context("could not create unique index")?;

        // Listings updated_at index with TTL
        let listings_index_model = IndexModel::builder()
            .keys(mongodb::bson::doc! {
                "updated_at": 1,
            })
            .options(IndexOptions::builder().expire_after(Duration::from_secs(3600 * 2)).build())
            .build();

        if let Err(e) = state.collection().create_index(listings_index_model.clone(), None).await {
            // Check for IndexOptionsConflict (Error code 85)
            // kind: CommandError(CommandError { code: 85, ... })
            let is_conflict = match &*e.kind {
                mongodb::error::ErrorKind::Command(cmd_err) => cmd_err.code == 85,
                _ => false,
            };

            if is_conflict {
                eprintln!("Index option conflict detected for 'updated_at'. Dropping old index and recreating...");
                // Drop the index by name. Default name for { updated_at: 1 } is "updated_at_1"
                state.collection().drop_index("updated_at_1", None).await
                    .context("could not drop conflicting updated_at index")?;
                
                // Retry creation
                state.collection().create_index(listings_index_model, None).await
                    .context("could not create updated_at index after restart")?;
                eprintln!("Index 'updated_at' recreated with new options.");
            } else {
                return Err(e).context("could not create updated_at index");
            }
        }

        // Parse collection indexes - content_id만 unique 인덱스
        state
            .parse_collection()
            .create_index(
                IndexModel::builder()
                    .keys(mongodb::bson::doc! {
                        "content_id": 1,
                    })
                    .options(IndexOptions::builder().unique(true).build())
                    .build(),
                None,
            )
            .await
            .context("could not create parse index")?;

        let stats_state = Arc::clone(&state);
        tokio::task::spawn(async move {
            loop {
                let all_time = match self::stats::get_stats(&*stats_state).await {
                    Ok(stats) => stats,
                    Err(e) => {
                        eprintln!("error generating stats: {:#?}", e);
                        continue;
                    }
                };

                let seven_days = match self::stats::get_stats_seven_days(&*stats_state).await {
                    Ok(stats) => stats,
                    Err(e) => {
                        eprintln!("error generating stats: {:#?}", e);
                        continue;
                    }
                };

                *stats_state.stats.write().await = Some(CachedStatistics {
                    all_time,
                    seven_days,
                });

                tokio::time::sleep(Duration::from_secs(60 * 60 * 12)).await;
            }
        });
        
        // Background FFLogs Parse Fetcher
        if state.fflogs_client.is_some() {
            let parse_state = Arc::clone(&state);
            tokio::task::spawn(async move {
                eprintln!("Starting FFLogs background service...");
                loop {
                   if let Err(e) = fetch_parses_task(&parse_state).await {
                       eprintln!("Error in FFLogs background task: {:?}", e);
                   }
                   tokio::time::sleep(Duration::from_secs(60)).await;
                }
            });
        } else {
            eprintln!("FFLogs client not configured, skipping background service.");
        }

        Ok(state)
    }

    pub fn collection(&self) -> Collection<ListingContainer> {
        self.mongo.database("rpf").collection("listings")
    }

    pub fn players_collection(&self) -> Collection<Player> {
        self.mongo.database("rpf").collection("players")
    }

    pub fn parse_collection(&self) -> Collection<crate::mongo::ParseCacheDoc> {
        self.mongo.database("rpf").collection("parses")
    }
}

/// 백그라운드 Parse 수집 태스크 (활성 파티 기반 + Zone별 배치 쿼리)
/// 
/// 1시간 이내 활성 파티의 멤버만 대상으로 파싱을 수집합니다.
/// Zone 단위로 조회하여 모든 encounter 데이터를 한 번에 저장합니다.
/// 배치 크기: 20명, Rate Limit: 1초/배치
async fn fetch_parses_task(state: &State) -> Result<()> {
    use std::collections::HashMap;
    
    let client = state.fflogs_client.as_ref().unwrap();
    
    // 1. 현재 활성 파티 목록 가져오기 (1시간 이내)
    let listings = get_current_listings(state.collection()).await?;
    
    // 2. 고난이도 파티만 필터링하고, Zone별로 플레이어 그룹화
    // Key: zone_id, Value: (difficulty_id, Vec<(content_id, name, server, region)>)
    let mut zone_players: HashMap<u32, (Option<u32>, Vec<(u64, String, String, &'static str)>)> = HashMap::new();
    
    for container in &listings {
        let duty_id = container.listing.duty as u16;
        
        // High-end + FFLogs 매핑 확인
        if !container.listing.high_end() {
            continue;
        }
        
        let fflogs_info = match crate::fflogs_mapping::get_fflogs_encounter(duty_id) {
            Some(info) => info,
            None => continue,
        };
        
        // 멤버 ContentID로 플레이어 정보 조회
        let member_ids: Vec<u64> = container.listing.member_content_ids
            .iter()
            .map(|&id| id as u64)
            .filter(|&id| id != 0)
            .collect();
        
        let players = get_players_by_content_ids(state.players_collection(), &member_ids).await?;
        
        let entry = zone_players.entry(fflogs_info.zone_id)
            .or_insert_with(|| (fflogs_info.difficulty_id, Vec::new()));
        
        for player in players {
            let region = crate::fflogs::get_region_from_server(&player.home_world_name());
            entry.1.push((player.content_id as u64, player.name.clone(), player.home_world_name().to_string(), region));
        }
    }
    
    // 중복 제거 (같은 플레이어가 여러 파티에 있을 수 있음)
    for (_, (_, players)) in zone_players.iter_mut() {
        players.sort_by_key(|p| p.0);
        players.dedup_by_key(|p| p.0);
    }
    
    let total_players: usize = zone_players.values().map(|(_, v)| v.len()).sum();
    eprintln!("[FFLogs] Found {} high-end listings, {} unique players across {} zones", 
        listings.len(), total_players, zone_players.len());
    
    let mut fetch_count = 0;
    let mut skip_count = 0;
    let mut saved_count = 0;
    let batch_size = 20;
    
    // Zone별로 처리
    for (zone_id, (difficulty_id, players)) in &zone_players {
        let zone_name = crate::fflogs_mapping::FFLOGS_ZONES
            .get(zone_id)
            .map(|z| z.name)
            .unwrap_or("Unknown Zone");
        
        // 캐시 확인 후 필터링: 해당 Zone의 캐시가 만료되지 않았는지 확인
        let mut players_to_fetch: Vec<&(u64, String, String, &'static str)> = Vec::new();
        
        for player in players {
            // Zone 캐시 조회
            let zone_cache = crate::mongo::get_zone_cache(
                state.parse_collection(), 
                player.0, 
                *zone_id
            ).await.ok().flatten();
            
            match &zone_cache {
                Some(cache) if !crate::mongo::is_zone_cache_expired(cache) => {
                    // 캐시가 유효함
                    skip_count += 1;
                }
                _ => {
                    // 캐시 없거나 만료됨
                    players_to_fetch.push(player);
                }
            }
        }
        
        if players_to_fetch.is_empty() {
            continue;
        }
        
        eprintln!("[FFLogs] {} - {} players to fetch", zone_name, players_to_fetch.len());
        
        let partition = crate::fflogs_mapping::FFLOGS_ZONES
            .get(zone_id)
            .map(|z| z.partition);
        
        // 배치 단위로 처리
        for chunk in players_to_fetch.chunks(batch_size) {
            let batch: Vec<(String, String, &'static str)> = chunk.iter()
                .map(|p| (p.1.clone(), p.2.clone(), p.3))
                .collect();
            
            // Rate Limit: 배치당 1초 대기
            tokio::time::sleep(Duration::from_secs(1)).await;
            
            // Zone 내 모든 encounter를 조회
            let results = client.get_batch_zone_all_parses(
                batch,
                *zone_id,
                *difficulty_id,
                partition
            ).await;
            
            fetch_count += 1;
            
            match results {
                Ok(batch_results) => {
                    for (idx, encounters) in &batch_results {
                        let player = chunk[*idx];
                        
                        // ZoneCache 생성
                        let mut encounter_map = HashMap::new();
                        for (enc_id, percentile) in encounters {
                            encounter_map.insert(
                                enc_id.to_string(),
                                crate::mongo::EncounterParse {
                                    percentile: *percentile,
                                    job_id: 0,
                                }
                            );
                        }
                        
                        let zone_cache = crate::mongo::ZoneCache {
                            fetched_at: chrono::Utc::now(),
                            encounters: encounter_map,
                        };
                        
                        // Zone 전체 upsert
                        let _ = crate::mongo::upsert_zone_cache(
                            state.parse_collection(),
                            player.0,
                            *zone_id,
                            &zone_cache
                        ).await;
                        
                        saved_count += encounters.len();
                    }
                },
                Err(e) => {
                    eprintln!("[FFLogs] Batch error for {}: {:?}", zone_name, e);
                }
            }
        }
    }
    
    eprintln!("[FFLogs] Cycle complete: {} batches, {} parses saved, {} skipped (cached)", 
        fetch_count, saved_count, skip_count);
    Ok(())
}

fn router(state: Arc<State>) -> BoxedFilter<(impl Reply,)> {
    index()
        .or(listings(Arc::clone(&state)))
        .or(contribute(Arc::clone(&state)))
        .or(contribute_multiple(Arc::clone(&state)))
        .or(contribute_players(Arc::clone(&state)))
        .or(contribute_detail(Arc::clone(&state)))
        .or(stats(Arc::clone(&state)))
        .or(stats_seven_days(Arc::clone(&state)))
        .or(assets())
        .or(api(Arc::clone(&state)))
        .boxed()
}

fn assets() -> BoxedFilter<(impl Reply,)> {
    warp::get()
        .and(warp::path("assets"))
        .and(
            icons()
                .or(minireset())
                .or(common_css())
                .or(listings_css())
                .or(listings_js())
                .or(stats_css())
                .or(stats_js())
                .or(d3())
                .or(pico())
                .or(common_js())
                .or(list_js())
                .or(translations_js()),
        )
        .boxed()
}

fn icons() -> BoxedFilter<(impl Reply,)> {
    warp::path("icons.svg")
        .and(warp::path::end())
        .and(warp::fs::file("./assets/icons.svg"))
        .boxed()
}

fn minireset() -> BoxedFilter<(impl Reply,)> {
    warp::path("minireset.css")
        .and(warp::path::end())
        .and(warp::fs::file("./assets/minireset.css"))
        .boxed()
}

fn common_css() -> BoxedFilter<(impl Reply,)> {
    warp::path("common.css")
        .and(warp::path::end())
        .and(warp::fs::file("./assets/common.css"))
        .boxed()
}

fn listings_css() -> BoxedFilter<(impl Reply,)> {
    warp::path("listings.css")
        .and(warp::path::end())
        .and(warp::fs::file("./assets/listings.css"))
        .boxed()
}

fn listings_js() -> BoxedFilter<(impl Reply,)> {
    warp::path("listings.js")
        .and(warp::path::end())
        .and(warp::fs::file("./assets/listings.js"))
        .boxed()
}

fn stats_css() -> BoxedFilter<(impl Reply,)> {
    warp::path("stats.css")
        .and(warp::path::end())
        .and(warp::fs::file("./assets/stats.css"))
        .boxed()
}

fn stats_js() -> BoxedFilter<(impl Reply,)> {
    warp::path("stats.js")
        .and(warp::path::end())
        .and(warp::fs::file("./assets/stats.js"))
        .boxed()
}

fn d3() -> BoxedFilter<(impl Reply,)> {
    warp::path("d3.js")
        .and(warp::path::end())
        .and(warp::fs::file("./assets/d3.v7.min.js"))
        .boxed()
}

fn pico() -> BoxedFilter<(impl Reply,)> {
    warp::path("pico.css")
        .and(warp::path::end())
        .and(warp::fs::file("./assets/pico.min.css"))
        .boxed()
}

fn common_js() -> BoxedFilter<(impl Reply,)> {
    warp::path("common.js")
        .and(warp::path::end())
        .and(warp::fs::file("./assets/common.js"))
        .boxed()
}

fn list_js() -> BoxedFilter<(impl Reply,)> {
    warp::path("list.js")
        .and(warp::path::end())
        .and(warp::fs::file("./assets/list.min.js"))
        .boxed()
}

fn translations_js() -> BoxedFilter<(impl Reply,)> {
    warp::path("translations.js")
        .and(warp::path::end())
        .and(warp::fs::file("./assets/translations.js"))
        .boxed()
}

fn index() -> BoxedFilter<(impl Reply,)> {
    let route = warp::path::end().map(|| warp::redirect(Uri::from_static("/listings")));
    warp::get().and(route).boxed()
}

fn listings(state: Arc<State>) -> BoxedFilter<(impl Reply,)> {
    async fn logic(
        state: Arc<State>,
        codes: Option<String>,
    ) -> std::result::Result<impl Reply, Infallible> {
        let lang = Language::from_codes(codes.as_deref());

        let res = get_current_listings(state.collection()).await;
        Ok(match res {
            Ok(mut containers) => {
                containers.sort_by(|a, b| {
                    a.time_left
                        .partial_cmp(&b.time_left)
                        .unwrap_or(Ordering::Equal)
                });

                containers.sort_by_key(|container| container.listing.pf_category());
                containers.reverse();

                containers.sort_by_key(|container| container.updated_minute);
                containers.reverse();

                // Collect all member IDs
                let all_content_ids: Vec<u64> = containers.iter()
                    .flat_map(|l| l.listing.member_content_ids.iter().map(|&id| id as u64))
                    .collect();
                
                // Fetch players
                let players_list = get_players_by_content_ids(state.players_collection(), &all_content_ids).await.unwrap_or_default();
                let players: HashMap<u64, crate::player::Player> = players_list.into_iter().map(|p| (p.content_id, p)).collect();

                // Match players to listings with job info
                let mut renderable_containers = Vec::new();

                for container in containers {
                    // Determine FFLogs Zone ID/Encounter ID
                    let duty_id = container.listing.duty as u16;
                    let high_end = container.listing.high_end();
                    let fflogs_info = if high_end {
                        crate::fflogs_mapping::get_fflogs_encounter(duty_id)
                    } else {
                        None
                    };
                    
                    let (zone_id, encounter_id) = if let Some(info) = fflogs_info {
                        (info.zone_id, info.encounter_id)
                    } else {
                        (0, 0)
                    };

                    let jobs = &container.listing.jobs_present;
                    let content_ids = &container.listing.member_content_ids;
                    
                    // Optimisation: Fetch zone caches for all members of this listing at once if it's a high-end duty
                    let zone_caches: HashMap<u64, crate::mongo::ZoneCache> = if zone_id > 0 {
                        let member_u64_ids: Vec<u64> = content_ids.iter().map(|&id| id as u64).collect();
                        crate::mongo::get_zone_caches(state.parse_collection(), &member_u64_ids, zone_id)
                            .await
                            .unwrap_or_default()
                    } else {
                        HashMap::new()
                    };

                    let members: Vec<crate::template::listings::RenderableMember> = content_ids.iter()
                        .enumerate()
                        .filter(|(_, id)| **id != 0) // 빈 슬롯 제외
                        .map(|(i, id)| {
                            let uid = *id as u64;
                            let job_id = jobs.get(i).copied().unwrap_or(0);
                            let player = players.get(&uid).cloned().unwrap_or(crate::player::Player {
                                content_id: uid,
                                name: "Unknown Member".to_string(),
                                home_world: 0,
                                last_seen: chrono::Utc::now(),
                                seen_count: 0,
                            });
                            
                            // Zone 캐시에서 해당 encounter의 parse 조회
                            let (percentile, color_class) = if let Some(zone_cache) = zone_caches.get(&uid) {
                                let enc_key = encounter_id.to_string();
                                if let Some(enc_parse) = zone_cache.encounters.get(&enc_key) {
                                    if enc_parse.percentile < 0.0 {
                                        (None, "parse-none".to_string())
                                    } else {
                                        (
                                            Some(enc_parse.percentile.round() as u8),
                                            crate::fflogs_mapping::percentile_color_class(enc_parse.percentile).to_string(),
                                        )
                                    }
                                } else {
                                    (None, "parse-none".to_string())
                                }
                            } else {
                                (None, "parse-none".to_string())
                            };

                            crate::template::listings::RenderableMember { 
                                job_id, 
                                player,
                                parse_percentile: percentile,
                                parse_color_class: color_class,
                            }
                        })
                        .collect();
                    
                    renderable_containers.push(crate::template::listings::RenderableListing {
                        container,
                        members,
                    });
                }

                ListingsTemplate { containers: renderable_containers, lang }
            }
            Err(e) => {
                eprintln!("{:#?}", e);
                ListingsTemplate {
                    containers: Default::default(),
                    lang,
                }
            }
        })
    }

    let route = warp::path("listings")
        .and(warp::path::end())
        .and(
            warp::cookie::<String>("lang")
                .or(warp::header::<String>("accept-language"))
                .unify()
                .map(Some)
                .or(warp::any().map(|| None))
                .unify(),
        )
        .and_then(move |codes: Option<String>| logic(Arc::clone(&state), codes));

    warp::get().and(route).boxed()
}

async fn stats_logic(
    state: Arc<State>,
    codes: Option<String>,
    seven_days: bool,
) -> std::result::Result<impl Reply, Infallible> {
    let lang = Language::from_codes(codes.as_deref());
    let stats = state.stats.read().await.clone();
    Ok(match stats {
        Some(stats) => StatsTemplate {
            stats: if seven_days {
                stats.seven_days
            } else {
                stats.all_time
            },
            lang,
        }.into_response(),
        None => "Stats haven't been calculated yet. Please wait :(".into_response(),
    })
}

fn stats(state: Arc<State>) -> BoxedFilter<(impl Reply,)> {
    let route = warp::path("stats")
        .and(warp::path::end())
        .and(
            warp::cookie::<String>("lang")
                .or(warp::header::<String>("accept-language"))
                .unify()
                .map(Some)
                .or(warp::any().map(|| None))
                .unify(),
        )
        .and_then(move |codes: Option<String>| stats_logic(Arc::clone(&state), codes, false));

    warp::get().and(route).boxed()
}

fn stats_seven_days(state: Arc<State>) -> BoxedFilter<(impl Reply,)> {
    let route = warp::path("stats")
        .and(warp::path("7days"))
        .and(warp::path::end())
        .and(
            warp::cookie::<String>("lang")
                .or(warp::header::<String>("accept-language"))
                .unify()
                .map(Some)
                .or(warp::any().map(|| None))
                .unify(),
        )
        .and_then(move |codes: Option<String>| stats_logic(Arc::clone(&state), codes, true));

    warp::get().and(route).boxed()
}

fn contribute(state: Arc<State>) -> BoxedFilter<(impl Reply,)> {
    async fn logic(
        state: Arc<State>,
        listing: PartyFinderListing,
    ) -> std::result::Result<impl Reply, Infallible> {
        if listing.seconds_remaining > 60 * 60 {
            return Ok("invalid listing".to_string());
        }

        let result = insert_listing(state.collection(), &listing).await;

        // publish listings to websockets
        let _ = state.listings_channel.send(vec![listing].into()); // ignore is OK, as `send` only fails when there are no receivers (which may happen)

        Ok(format!("{:#?}", result))
    }

    let route = warp::path("contribute")
        .and(warp::path::end())
        .and(warp::body::json())
        .and_then(move |listing: PartyFinderListing| logic(Arc::clone(&state), listing));
    warp::post().and(route).boxed()
}

fn contribute_multiple(state: Arc<State>) -> BoxedFilter<(impl Reply,)> {
    async fn logic(
        state: Arc<State>,
        listings: Vec<PartyFinderListing>,
    ) -> std::result::Result<impl Reply, Infallible> {
        let total = listings.len();
        let mut successful = 0;

        for listing in &listings {
            if listing.seconds_remaining > 60 * 60 {
                continue;
            }

            let result = insert_listing(state.collection(), listing).await;
            if result.is_ok() {
                successful += 1;
            } else {
                eprintln!("{:#?}", result);
            }
        }

        let _ = state.listings_channel.send(listings.into()); // ignore is OK, as `send` only fails when there are no receivers (which may happen)

        Ok(format!("{}/{} updated", successful, total))
    }

    let route = warp::path("contribute")
        .and(warp::path("multiple"))
        .and(warp::path::end())
        .and(warp::body::json())
        .and_then(move |listings: Vec<PartyFinderListing>| logic(Arc::clone(&state), listings));
    warp::post().and(route).boxed()
}

fn contribute_players(state: Arc<State>) -> BoxedFilter<(impl Reply,)> {
    async fn logic(
        state: Arc<State>,
        players: Vec<UploadablePlayer>,
    ) -> std::result::Result<impl Reply, Infallible> {
        let total = players.len();
        let result = upsert_players(state.players_collection(), &players).await;

        match result {
            Ok(successful) => Ok(format!("{}/{} players updated", successful, total)),
            Err(e) => {
                eprintln!("error upserting players: {:#?}", e);
                Ok(format!("0/{} players updated (error)", total))
            }
        }
    }

    let route = warp::path("contribute")
        .and(warp::path("players"))
        .and(warp::path::end())
        .and(warp::body::json())
        .and_then(move |players: Vec<UploadablePlayer>| logic(Arc::clone(&state), players));
    warp::post().and(route).boxed()
}

/// 파티 상세 정보 (멤버 ContentId 목록)
#[derive(Debug, serde::Deserialize)]
struct UploadablePartyDetail {
    listing_id: u32,
    leader_content_id: u64,
    leader_name: String,
    home_world: u16,
    member_content_ids: Vec<u64>,
}

fn contribute_detail(state: Arc<State>) -> BoxedFilter<(impl Reply,)> {
    async fn logic(
        state: Arc<State>,
        detail: UploadablePartyDetail,
    ) -> std::result::Result<impl Reply, Infallible> {
        // 멤버 ContentId를 플레이어 테이블에도 upsert (이름은 null이지만 ContentId는 알게 됨)
        // 또한 listing에 member_content_ids를 업데이트
        
        // 리더 정보를 플레이어로 저장
        if detail.leader_content_id != 0 && !detail.leader_name.is_empty() && detail.home_world < 1000 {
            let leader = crate::player::UploadablePlayer {
                content_id: detail.leader_content_id,
                name: detail.leader_name.clone(),
                home_world: detail.home_world,
            };
            let upsert_res = upsert_players(state.players_collection(), &[leader]).await;
            eprintln!("Upserted leader {}: {:?}", detail.leader_content_id, upsert_res);
        } else {
            eprintln!("Skipping leader upsert: ID={} Name='{}' World={}", detail.leader_content_id, detail.leader_name, detail.home_world);
        }

        // listing에 member_content_ids 저장
        let member_count = detail.member_content_ids.len();
        let member_ids_i64: Vec<i64> = detail.member_content_ids.iter().map(|&id| id as i64).collect();

        let update_result = state.collection()
            .update_one(
                mongodb::bson::doc! { "listing.id": detail.listing_id },
                mongodb::bson::doc! {
                    "$set": {
                        "listing.member_content_ids": member_ids_i64,
                    }
                },
                None,
            )
            .await;

        eprintln!("Updated listing {} members: {:?}", detail.listing_id, update_result);

        Ok(warp::reply::json(&"ok"))
    }

    let route = warp::path("contribute")
        .and(warp::path("detail"))
        .and(warp::path::end())
        .and(warp::body::json())
        .and_then(move |detail: UploadablePartyDetail| logic(Arc::clone(&state), detail));
    warp::post().and(route).boxed()
}
