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

            // Collect all member IDs + leader IDs
            let mut all_content_ids: Vec<u64> = containers.iter()
                .flat_map(|l| {
                    let member_ids = l.listing.member_content_ids.iter().map(|&id| id as u64);
                    let leader_id = std::iter::once(l.listing.leader_content_id);
                    member_ids.chain(leader_id)
                })
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
                
                let (zone_id, encounter_id, secondary_encounter_id) = if let Some(info) = fflogs_info {
                    (info.zone_id, info.encounter_id, info.secondary_encounter_id)
                } else {
                    (0, 0, None)
                };

                let jobs = &container.listing.jobs_present;
                let content_ids = &container.listing.member_content_ids;
                
                let zone_key = zone_id.to_string();

                let members: Vec<crate::template::listings::RenderableMember> = content_ids.iter()
                    .enumerate()
                    .filter(|(_, id)| **id != 0) // 빈 슬롯 제외
                    .filter_map(|(i, id)| {
                        let uid = *id as u64;
                        let job_id = jobs.get(i).copied().unwrap_or(0);
                        let player = players.get(&uid).cloned().unwrap_or(crate::player::Player {
                            content_id: uid,
                            name: "Unknown Member".to_string(),
                            home_world: 0,
                            last_seen: chrono::Utc::now(),
                            seen_count: 0,
                        });
                        
                        // 잡 정보가 없는 멤버는 표시하지 않음 (Ghost Member 방지)
                        // 리스팅 정보(jobs)와 세부 정보(content_ids) 간의 불일치 시, 리스팅 정보를 신뢰함
                        if job_id == 0 {
                            return None;
                        }

                        // Parse Data (P1 & P2)
                        let mut p1_percentile = None;
                        let mut p1_class = "parse-none".to_string();
                        let mut p2_percentile = None;
                        let mut p2_class = "parse-none".to_string();

                        if zone_id > 0 {
                            if let Some(doc) = all_parse_docs.get(&uid) {
                                if let Some(zone_cache) = doc.zones.get(&zone_key) {
                                    // Primary (P1)
                                    if let Some(enc_parse) = zone_cache.encounters.get(&encounter_id.to_string()) {
                                        if enc_parse.percentile >= 0.0 {
                                            p1_percentile = Some(enc_parse.percentile as u8);
                                            p1_class = crate::fflogs_mapping::percentile_color_class(enc_parse.percentile).to_string();
                                        }
                                    }
                                    
                                    // Secondary (P2)
                                    if let Some(sec_id) = secondary_encounter_id {
                                        if let Some(enc_parse) = zone_cache.encounters.get(&sec_id.to_string()) {
                                            if enc_parse.percentile >= 0.0 {
                                                p2_percentile = Some(enc_parse.percentile as u8);
                                                p2_class = crate::fflogs_mapping::percentile_color_class(enc_parse.percentile).to_string();
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        Some(crate::template::listings::RenderableMember { 
                            job_id, 
                            player,
                            parse_percentile: p1_percentile,
                            parse_color_class: p1_class,
                            secondary_parse_percentile: p2_percentile,
                            secondary_parse_color_class: p2_class,
                            has_secondary: secondary_encounter_id.is_some(),
                        })
                    })
                    .collect();
                
                // 파티장 로그 계산 (leader_content_id 사용)
                let leader_content_id = container.listing.leader_content_id;
                let mut leader_p1_percentile = None;
                let mut leader_p1_class = "parse-none".to_string();
                let mut leader_p2_percentile = None;
                let mut leader_p2_class = "parse-none".to_string();

                if zone_id > 0 && leader_content_id != 0 {
                    if let Some(doc) = all_parse_docs.get(&leader_content_id) {
                        if let Some(zone_cache) = doc.zones.get(&zone_key) {
                            // Primary (P1)
                            if let Some(enc_parse) = zone_cache.encounters.get(&encounter_id.to_string()) {
                                if enc_parse.percentile >= 0.0 {
                                    leader_p1_percentile = Some(enc_parse.percentile as u8);
                                    leader_p1_class = crate::fflogs_mapping::percentile_color_class(enc_parse.percentile).to_string();
                                }
                            }
                            
                            // Secondary (P2)
                            if let Some(sec_id) = secondary_encounter_id {
                                if let Some(enc_parse) = zone_cache.encounters.get(&sec_id.to_string()) {
                                    if enc_parse.percentile >= 0.0 {
                                        leader_p2_percentile = Some(enc_parse.percentile as u8);
                                        leader_p2_class = crate::fflogs_mapping::percentile_color_class(enc_parse.percentile).to_string();
                                    }
                                }
                            }
                        }
                    }
                }

                renderable_containers.push(crate::template::listings::RenderableListing {
                    container,
                    members,
                    leader_parse_percentile: leader_p1_percentile,
                    leader_parse_color_class: leader_p1_class,
                    leader_secondary_parse_percentile: leader_p2_percentile,
                    leader_secondary_parse_color_class: leader_p2_class,
                    leader_has_secondary: secondary_encounter_id.is_some(),
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

    // listing에 member_content_ids 및 leader_content_id 저장
    let member_ids_i64: Vec<i64> = detail.member_content_ids.iter().map(|&id| id as i64).collect();

    let update_result = state.collection()
        .update_one(
            doc! { "listing.id": detail.listing_id },
            doc! {
                "$set": {
                    "listing.member_content_ids": member_ids_i64,
                    "listing.leader_content_id": detail.leader_content_id as i64,
                }
            },
            None,
        )
        .await;

    tracing::debug!("Updated listing {} members: {:?}", detail.listing_id, update_result);

    Ok(warp::reply::json(&"ok"))
}
