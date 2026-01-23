use std::{cmp::Ordering, collections::HashMap, convert::Infallible, sync::Arc};
use warp::Reply;
use mongodb::bson::doc;

use crate::listing::PartyFinderListing;

use crate::mongo::{get_current_listings, insert_listing, upsert_players, get_players_by_content_ids, get_parse_docs};
use crate::player::UploadablePlayer;
use crate::{
    ffxiv::Language,
    template::listings::ListingsTemplate,
    template::stats::StatsTemplate,
};
use super::State;

pub async fn listings_handler(
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
            let mut all_content_ids: Vec<u64> = containers.iter()
                .flat_map(|l| l.listing.member_content_ids.iter().map(|&id| id as u64))
                .filter(|&id| id != 0)
                .collect();
            all_content_ids.sort_unstable();
            all_content_ids.dedup();
            
            // Fetch players
            let players_list = get_players_by_content_ids(state.players_collection(), &all_content_ids).await.unwrap_or_default();
            let players: HashMap<u64, crate::player::Player> = players_list.into_iter().map(|p| (p.content_id, p)).collect();

            // Optimisation: Pre-fetch all parse docs for all visible players
            let all_parse_docs = get_parse_docs(state.parse_collection(), &all_content_ids).await.unwrap_or_default();

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
                
                let zone_key = zone_id.to_string();

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
                        let (percentile, color_class) = if zone_id > 0 {
                            if let Some(doc) = all_parse_docs.get(&uid) {
                                if let Some(zone_cache) = doc.zones.get(&zone_key) {
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
            tracing::error!("Failed to get listings: {:#?}", e);
            ListingsTemplate {
                containers: Default::default(),
                lang,
            }
        }
    })
}

pub async fn stats_handler(
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

pub async fn contribute_handler(
    state: Arc<State>,
    listing: PartyFinderListing,
) -> std::result::Result<impl Reply, Infallible> {
    if listing.seconds_remaining > 60 * 60 {
        return Ok("invalid listing".to_string());
    }

    let result = insert_listing(state.collection(), &listing).await;

    // publish listings to websockets
    let _ = state.listings_channel.send(vec![listing].into()); 
    Ok(format!("{:#?}", result))
}

pub async fn contribute_multiple_handler(
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
            tracing::warn!("Failed to insert listing: {:#?}", result);
        }
    }

    let _ = state.listings_channel.send(listings.into());
    Ok(format!("{}/{} updated", successful, total))
}

pub async fn contribute_players_handler(
    state: Arc<State>,
    players: Vec<UploadablePlayer>,
) -> std::result::Result<impl Reply, Infallible> {
    let total = players.len();
    let result = upsert_players(state.players_collection(), &players).await;

    match result {
        Ok(successful) => Ok(format!("{}/{} players updated", successful, total)),
        Err(e) => {
            tracing::error!("error upserting players: {:#?}", e);
            Ok(format!("0/{} players updated (error)", total))
        }
    }
}

/// 파티 상세 정보 (멤버 ContentId 목록)
#[derive(Debug, serde::Deserialize)]
pub struct UploadablePartyDetail {
    pub listing_id: u32,
    pub leader_content_id: u64,
    pub leader_name: String,
    pub home_world: u16,
    pub member_content_ids: Vec<u64>,
}

pub async fn contribute_detail_handler(
    state: Arc<State>,
    detail: UploadablePartyDetail,
) -> std::result::Result<impl Reply, Infallible> {
    // 리더 정보를 플레이어로 저장
    if detail.leader_content_id != 0 && !detail.leader_name.is_empty() && detail.home_world < 1000 {
        let leader = crate::player::UploadablePlayer {
            content_id: detail.leader_content_id,
            name: detail.leader_name.clone(),
            home_world: detail.home_world,
        };
        let upsert_res = upsert_players(state.players_collection(), &[leader]).await;
        tracing::debug!("Upserted leader {}: {:?}", detail.leader_content_id, upsert_res);
    } else {
        tracing::debug!("Skipping leader upsert: ID={} Name='{}' World={}", detail.leader_content_id, detail.leader_name, detail.home_world);
    }

    // listing에 member_content_ids 저장
    let member_ids_i64: Vec<i64> = detail.member_content_ids.iter().map(|&id| id as i64).collect();

    let update_result = state.collection()
        .update_one(
            doc! { "listing.id": detail.listing_id },
            doc! {
                "$set": {
                    "listing.member_content_ids": member_ids_i64,
                }
            },
            None,
        )
        .await;

    tracing::debug!("Updated listing {} members: {:?}", detail.listing_id, update_result);

    Ok(warp::reply::json(&"ok"))
}
