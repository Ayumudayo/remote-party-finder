use std::{collections::HashMap, sync::Arc, time::Duration};
use anyhow::Result;

use crate::mongo::{get_current_listings, get_players_by_content_ids};
use crate::stats::CachedStatistics;
use super::State;

pub fn spawn_stats_task(state: Arc<State>) {
    let stats_state = Arc::clone(&state);
    tokio::task::spawn(async move {
        loop {
            let all_time = match crate::stats::get_stats(&*stats_state).await {
                Ok(stats) => stats,
                Err(e) => {
                    tracing::error!("error generating stats: {:#?}", e);
                    continue;
                }
            };

            let seven_days = match crate::stats::get_stats_seven_days(&*stats_state).await {
                Ok(stats) => stats,
                Err(e) => {
                    tracing::error!("error generating stats: {:#?}", e);
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
}

pub fn spawn_fflogs_task(state: Arc<State>) {
    if state.fflogs_client.is_some() {
        let parse_state = Arc::clone(&state);
        tokio::task::spawn(async move {
            tracing::info!("Starting FFLogs background service...");
            loop {
               if let Err(e) = fetch_parses_task(&parse_state).await {
                   tracing::error!("Error in FFLogs background task: {:?}", e);
               }
               tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
    } else {
        tracing::info!("FFLogs client not configured, skipping background service.");
    }
}

/// 백그라운드 Parse 수집 태스크 (활성 파티 기반 + Zone별 배치 쿼리)
/// 
/// 1시간 이내 활성 파티의 멤버만 대상으로 파싱을 수집합니다.
/// Zone 단위로 조회하여 모든 encounter 데이터를 한 번에 저장합니다.
/// 배치 크기: 20명, Rate Limit: 1초/배치
async fn fetch_parses_task(state: &State) -> Result<()> {
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
        
        let fflogs_info = match crate::fflogs::mapping::get_fflogs_encounter(duty_id) {
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
    tracing::info!("[FFLogs] Found {} high-end listings, {} unique players across {} zones", 
        listings.len(), total_players, zone_players.len());
    
    let mut fetch_count = 0;
    let mut skip_count = 0;
    let mut saved_count = 0;
    let batch_size = 20;
    
    // Zone별로 처리
    for (zone_id, (difficulty_id, players)) in &zone_players {
        let zone_name = crate::fflogs::mapping::FFLOGS_ZONES
            .get(zone_id)
            .map(|z| z.name)
            .unwrap_or("Unknown Zone");
        
        // 배치로 Zone 캐시 일괄 조회 (N+1 쿼리 방지)
        let content_ids: Vec<u64> = players.iter().map(|p| p.0).collect();
        let cached_zones = crate::mongo::get_zone_caches(
            state.parse_collection(),
            &content_ids,
            *zone_id
        ).await.unwrap_or_default();
        
        // 캐시 확인 후 필터링: 해당 Zone의 캐시가 만료되지 않았는지 확인
        let mut players_to_fetch: Vec<&(u64, String, String, &'static str)> = Vec::new();
        
        for player in players {
            match cached_zones.get(&player.0) {
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
        
        tracing::info!("[FFLogs] {} - {} players to fetch", zone_name, players_to_fetch.len());
        
        let partition = crate::fflogs::mapping::FFLOGS_ZONES
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
                    tracing::warn!("[FFLogs] Batch error for {}: {:?}", zone_name, e);
                }
            }
        }
    }
    
    tracing::info!("[FFLogs] Cycle complete: {} batches, {} parses saved, {} skipped (cached)", 
        fetch_count, saved_count, skip_count);
    Ok(())
}
