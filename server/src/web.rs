use std::{cmp::Ordering, collections::HashMap, convert::Infallible, sync::Arc, time::Duration};

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
}

impl State {
    pub async fn new(config: Arc<Config>) -> Result<Arc<Self>> {
        let mongo = MongoClient::with_uri_str(&config.mongo.url)
            .await
            .context("could not create mongodb client")?;

        let (tx, _) = tokio::sync::broadcast::channel(16);
        let state = Arc::new(Self {
            mongo,
            stats: Default::default(),
            listings_channel: tx,
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

        state
            .collection()
            .create_index(
                IndexModel::builder()
                    .keys(mongodb::bson::doc! {
                        "updated_at": 1,
                    })
                    .build(),
                None,
            )
            .await
            .context("could not create updated_at index")?;

        let task_state = Arc::clone(&state);
        tokio::task::spawn(async move {
            loop {
                let all_time = match self::stats::get_stats(&*task_state).await {
                    Ok(stats) => stats,
                    Err(e) => {
                        eprintln!("error generating stats: {:#?}", e);
                        continue;
                    }
                };

                let seven_days = match self::stats::get_stats_seven_days(&*task_state).await {
                    Ok(stats) => stats,
                    Err(e) => {
                        eprintln!("error generating stats: {:#?}", e);
                        continue;
                    }
                };

                *task_state.stats.write().await = Some(CachedStatistics {
                    all_time,
                    seven_days,
                });

                tokio::time::sleep(Duration::from_secs(60 * 60 * 12)).await;
            }
        });

        Ok(state)
    }

    pub fn collection(&self) -> Collection<ListingContainer> {
        self.mongo.database("rpf").collection("listings")
    }

    pub fn players_collection(&self) -> Collection<Player> {
        self.mongo.database("rpf").collection("players")
    }
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
                .or(list_js()),
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
                let containers: Vec<crate::template::listings::RenderableListing> = containers.into_iter()
                    .map(|container| {
                        // jobs_present와 member_content_ids는 같은 인덱스로 매칭됨
                        let jobs = &container.listing.jobs_present;
                        let content_ids = &container.listing.member_content_ids;
                        
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
                                crate::template::listings::RenderableMember { job_id, player }
                            })
                            .collect();
                        
                        crate::template::listings::RenderableListing {
                            container,
                            members,
                        }
                    })
                    .collect();

                ListingsTemplate { containers, lang }
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
