//! FFLogs API v2 (GraphQL) 클라이언트
//!
//! OAuth2 Client Credentials Flow를 사용하여 FFLogs API에 접근합니다.
//! 캐릭터의 Zone Rankings를 조회하여 Best Percentile을 가져옵니다.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::config::FFLogs as FFLogsConfig;

/// FFLogs API 토큰 엔드포인트
const OAUTH_TOKEN_URL: &str = "https://www.fflogs.com/oauth/token";
/// FFLogs GraphQL API 엔드포인트
const GRAPHQL_URL: &str = "https://www.fflogs.com/api/v2/client";

/// FFLogs API 클라이언트
pub struct FFLogsClient {
    config: FFLogsConfig,
    http: reqwest::Client,
    token: Arc<RwLock<Option<AccessToken>>>,
}

/// OAuth2 Access Token
#[derive(Debug, Clone)]
struct AccessToken {
    token: String,
    expires_at: DateTime<Utc>,
}

/// OAuth2 토큰 응답
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: i64,
    token_type: String,
}

/// GraphQL 응답
#[derive(Debug, Deserialize)]
struct GraphQLResponse<T> {
    data: Option<T>,
    errors: Option<Vec<GraphQLError>>,
}

#[derive(Debug, Deserialize)]
struct GraphQLError {
    message: String,
}

/// 캐릭터 조회 응답
#[derive(Debug, Deserialize)]
struct CharacterData {
    #[serde(rename = "characterData")]
    character_data: Option<CharacterWrapper>,
}

#[derive(Debug, Deserialize)]
struct CharacterWrapper {
    character: Option<Character>,
}

#[derive(Debug, Deserialize)]
struct Character {
    #[serde(rename = "zoneRankings")]
    zone_rankings: Option<serde_json::Value>,
}

/// Parse 점수 결과
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParseInfo {
    /// 캐릭터 이름
    pub name: String,
    /// 서버 이름
    pub server: String,
    /// Zone ID
    pub zone_id: u32,
    /// Encounter ID
    pub encounter_id: u32,
    /// 직업 이름
    pub job: String,
    /// Best Percentile (0-100)
    pub percentile: f32,
    /// 조회 시각
    pub fetched_at: DateTime<Utc>,
}

impl FFLogsClient {
    /// 새 FFLogs 클라이언트 생성
    pub fn new(config: FFLogsConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
            token: Arc::new(RwLock::new(None)),
        }
    }

    /// 유효한 액세스 토큰 가져오기 (필요시 갱신)
    async fn get_token(&self) -> anyhow::Result<String> {
        // 기존 토큰 확인
        {
            let token_guard = self.token.read().await;
            if let Some(ref token) = *token_guard {
                // 만료 5분 전까지 유효
                if token.expires_at > Utc::now() + chrono::Duration::minutes(5) {
                    return Ok(token.token.clone());
                }
            }
        }

        // 새 토큰 요청
        let response = self
            .http
            .post(OAUTH_TOKEN_URL)
            .basic_auth(&self.config.client_id, Some(&self.config.client_secret))
            .form(&[("grant_type", "client_credentials")])
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("FFLogs OAuth failed: {} - {}", status, body);
        }

        let token_response: TokenResponse = response.json().await?;
        
        let access_token = AccessToken {
            token: token_response.access_token.clone(),
            expires_at: Utc::now() + chrono::Duration::seconds(token_response.expires_in),
        };

        // 토큰 저장
        {
            let mut token_guard = self.token.write().await;
            *token_guard = Some(access_token);
        }

        Ok(token_response.access_token)
    }

    /// GraphQL 쿼리 실행
    async fn query<T: for<'de> Deserialize<'de>>(
        &self,
        query: &str,
        variables: serde_json::Value,
    ) -> anyhow::Result<T> {
        let token = self.get_token().await?;

        let response = self
            .http
            .post(GRAPHQL_URL)
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "query": query,
                "variables": variables
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("FFLogs API error: {} - {}", status, body);
        }

        let result: GraphQLResponse<T> = response.json().await?;

        if let Some(errors) = result.errors {
            if !errors.is_empty() {
                anyhow::bail!("FFLogs GraphQL errors: {:?}", errors);
            }
        }

        result.data.ok_or_else(|| anyhow::anyhow!("No data in response"))
    }

    /// 캐릭터의 Zone Rankings 조회
    ///
    /// # Arguments
    /// * `name` - 캐릭터 이름
    /// * `server` - 서버 이름 (예: "Mana", "Tonberry")
    /// * `region` - 리전 (예: "JP", "NA", "EU")
    /// * `zone_id` - FFLogs Zone ID
    ///
    /// # Returns
    /// 해당 Zone에서의 Best Percentile (없으면 None)
    pub async fn get_character_zone_rankings(
        &self,
        name: &str,
        server: &str,
        region: &str,
        zone_id: u32,
    ) -> anyhow::Result<Option<f32>> {
        let query = r#"
            query($name: String!, $server: String!, $region: String!, $zoneID: Int!) {
                characterData {
                    character(name: $name, serverSlug: $server, serverRegion: $region) {
                        zoneRankings(zoneID: $zoneID)
                    }
                }
            }
        "#;

        let variables = serde_json::json!({
            "name": name,
            "server": server.to_lowercase(),
            "region": region,
            "zoneID": zone_id
        });

        let result: CharacterData = self.query(query, variables).await?;

        // zoneRankings에서 bestPerformanceAverage 추출
        if let Some(wrapper) = result.character_data {
            if let Some(character) = wrapper.character {
                if let Some(rankings) = character.zone_rankings {
                    // zoneRankings는 복잡한 구조, bestPerformanceAverage 추출
                    if let Some(best) = rankings.get("bestPerformanceAverage") {
                        if let Some(percentile) = best.as_f64() {
                            return Ok(Some(percentile as f32));
                        }
                    }
                }
            }
        }

        Ok(None)
    }

    /// 캐릭터의 특정 Encounter Best Parse 조회
    pub async fn get_encounter_best_parse(
        &self,
        name: &str,
        server: &str,
        region: &str,
        zone_id: u32,
        encounter_id: u32,
        difficulty_id: Option<u32>,
        partition: Option<u32>,
    ) -> anyhow::Result<Option<f32>> {
        // Plugin Logic Mimic:
        // Use zoneRankings with difficulty, metric, partition, timeframe.
        // We filter by encounterID client-side because zoneRankings returns all encounters in the zone.
        
        let query = r#"
            query($name: String!, $server: String!, $region: String!, $zoneID: Int!, $difficulty: Int, $partition: Int) {
                characterData {
                    character(name: $name, serverSlug: $server, serverRegion: $region) {
                        zoneRankings(zoneID: $zoneID, difficulty: $difficulty, metric: rdps, partition: $partition, timeframe: Historical)
                    }
                }
            }
        "#;

        let variables = serde_json::json!({
            "name": name,
            "server": server.to_lowercase(),
            "region": region,
            "zoneID": zone_id,
            "difficulty": difficulty_id, // e.g. 100 or 101. If None, API default (usually all?)
            "partition": partition // e.g. 1 (Standard)
        });

        // Debug logging for query variables
        // eprintln!("FFLogs Query Vars: {:?}", variables);

        let result: CharacterData = self.query(query, variables).await?;

        if let Some(wrapper) = result.character_data {
            if let Some(character) = wrapper.character {
                if let Some(rankings_val) = character.zone_rankings {
                    // Case: rankings array
                    if let Some(rankings_array) = rankings_val.get("rankings") {
                        if let Some(arr) = rankings_array.as_array() {
                            for item in arr {
                                // encounter field check
                                if let Some(encounter) = item.get("encounter") {
                                    if let Some(id) = encounter.get("id").and_then(|v| v.as_u64()) {
                                        if id as u32 == encounter_id {
                                            // Found matching encounter
                                            if let Some(percent) = item.get("rankPercent").and_then(|v| v.as_f64()) {
                                                return Ok(Some(percent as f32));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // Debug if needed
                    // eprintln!("Debug zoneRankings: {:?}", rankings_val);
                }
            }
        }

        Ok(None)
    }

    /// 여러 캐릭터의 Encounter Parse를 한 번에 조회 (배치 쿼리)
    /// 
    /// GraphQL alias를 사용하여 한 번의 API 호출로 여러 캐릭터를 조회합니다.
    /// 최대 20명까지 한 번에 조회 가능합니다.
    /// 
    /// # Returns
    /// Vec<(player_index, Option<f32>)> - 각 플레이어의 인덱스와 파싱 결과
    pub async fn get_batch_encounter_parses(
        &self,
        players: Vec<(String, String, &str)>, // (name, server, region)
        zone_id: u32,
        encounter_id: u32,
        difficulty_id: Option<u32>,
        partition: Option<u32>,
    ) -> anyhow::Result<Vec<(usize, Option<f32>)>> {
        if players.is_empty() {
            return Ok(Vec::new());
        }

        // 동적 GraphQL 쿼리 생성
        let mut query_parts = Vec::new();
        for (i, (name, server, region)) in players.iter().enumerate() {
            let alias = format!("char{}", i);
            let server_lower = server.to_lowercase();
            
            // difficulty와 partition을 조건부로 추가
            let difficulty_arg = difficulty_id.map(|d| format!(", difficulty: {}", d)).unwrap_or_default();
            let partition_arg = partition.map(|p| format!(", partition: {}", p)).unwrap_or_default();
            
            query_parts.push(format!(
                r#"{}: character(name: "{}", serverSlug: "{}", serverRegion: "{}") {{
                    zoneRankings(zoneID: {}{}{}, metric: rdps, timeframe: Historical)
                }}"#,
                alias, name, server_lower, region, zone_id, difficulty_arg, partition_arg
            ));
        }

        let query = format!(
            r#"query {{ characterData {{ {} }} }}"#,
            query_parts.join("\n")
        );

        let token = self.get_token().await?;

        let response = self
            .http
            .post(GRAPHQL_URL)
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "query": query
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("FFLogs API error: {} - {}", status, body);
        }

        let result: serde_json::Value = response.json().await?;

        // 결과 파싱
        let mut results = Vec::new();
        
        if let Some(errors) = result.get("errors") {
            if errors.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
                // 에러가 있어도 부분 결과는 처리 가능
                // eprintln!("[FFLogs] Batch query had errors: {:?}", errors);
            }
        }

        if let Some(data) = result.get("data").and_then(|d| d.get("characterData")) {
            for (i, _) in players.iter().enumerate() {
                let alias = format!("char{}", i);
                
                let percentile = data
                    .get(&alias)
                    .and_then(|char| char.get("zoneRankings"))
                    .and_then(|zr| zr.get("rankings"))
                    .and_then(|rankings| rankings.as_array())
                    .and_then(|arr| {
                        for item in arr {
                            if let Some(enc) = item.get("encounter") {
                                if let Some(id) = enc.get("id").and_then(|v| v.as_u64()) {
                                    if id as u32 == encounter_id {
                                        return item.get("rankPercent").and_then(|v| v.as_f64()).map(|p| p as f32);
                                    }
                                }
                            }
                        }
                        None
                    });
                
                results.push((i, percentile));
            }
        } else {
            // No data at all
            for i in 0..players.len() {
                results.push((i, None));
            }
        }

        Ok(results)
    }

    /// 여러 캐릭터의 Zone 내 모든 Encounter Parse를 한 번에 조회 (배치 쿼리)
    /// 
    /// GraphQL alias를 사용하여 한 번의 API 호출로 여러 캐릭터를 조회합니다.
    /// Zone 내 모든 encounter의 rankings를 반환합니다.
    /// 
    /// # Returns
    /// Vec<(player_index, Vec<(encounter_id, percentile)>)> - 각 플레이어의 모든 encounter 결과
    pub async fn get_batch_zone_all_parses(
        &self,
        players: Vec<(String, String, &str)>, // (name, server, region)
        zone_id: u32,
        difficulty_id: Option<u32>,
        partition: Option<u32>,
    ) -> anyhow::Result<Vec<(usize, Vec<(u32, f32)>)>> {
        if players.is_empty() {
            return Ok(Vec::new());
        }

        // 동적 GraphQL 쿼리 생성
        let mut query_parts = Vec::new();
        for (i, (name, server, region)) in players.iter().enumerate() {
            let alias = format!("char{}", i);
            let server_lower = server.to_lowercase();
            
            let difficulty_arg = difficulty_id.map(|d| format!(", difficulty: {}", d)).unwrap_or_default();
            let partition_arg = partition.map(|p| format!(", partition: {}", p)).unwrap_or_default();
            
            query_parts.push(format!(
                r#"{}: character(name: "{}", serverSlug: "{}", serverRegion: "{}") {{
                    zoneRankings(zoneID: {}{}{}, metric: rdps, timeframe: Historical)
                }}"#,
                alias, name, server_lower, region, zone_id, difficulty_arg, partition_arg
            ));
        }

        let query = format!(
            r#"query {{ characterData {{ {} }} }}"#,
            query_parts.join("\n")
        );

        let token = self.get_token().await?;

        let response = self
            .http
            .post(GRAPHQL_URL)
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "query": query
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("FFLogs API error: {} - {}", status, body);
        }

        let result: serde_json::Value = response.json().await?;

        // 결과 파싱 - Zone 내 모든 encounter 추출
        let mut results = Vec::new();
        
        if let Some(errors) = result.get("errors") {
            if errors.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
                // 에러가 있어도 부분 결과는 처리 가능
            }
        }

        if let Some(data) = result.get("data").and_then(|d| d.get("characterData")) {
            for (i, _) in players.iter().enumerate() {
                let alias = format!("char{}", i);
                
                let encounters: Vec<(u32, f32)> = data
                    .get(&alias)
                    .and_then(|char| char.get("zoneRankings"))
                    .and_then(|zr| zr.get("rankings"))
                    .and_then(|rankings| rankings.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|item| {
                                let enc_id = item.get("encounter")
                                    .and_then(|e| e.get("id"))
                                    .and_then(|v| v.as_u64())
                                    .map(|id| id as u32)?;
                                let percentile = item.get("rankPercent")
                                    .and_then(|v| v.as_f64())
                                    .map(|p| p as f32)?;
                                Some((enc_id, percentile))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                
                results.push((i, encounters));
            }
        } else {
            // No data at all
            for i in 0..players.len() {
                results.push((i, Vec::new()));
            }
        }

        Ok(results)
    }
}

/// 서버 이름에서 리전 추출
/// 서버 이름에서 리전 추출
pub fn get_region_from_server(server: &str) -> &'static str {
    // JP (Elemental, Gaia, Mana, Meteor)
    let jp_servers = [
        // Elemental
        "Aegis", "Atomos", "Carbuncle", "Garuda", "Gungnir", "Kujata", "Ramuh", "Tonberry", "Typhon", "Unicorn",
        // Gaia
        "Alexander", "Bahamut", "Durandal", "Fenrir", "Ifrit", "Ridill", "Tiamat", "Ultima", "Valefor", "Yojimbo", "Zeromus",
        // Mana
        "Anima", "Asura", "Chocobo", "Hades", "Ixion", "Masamune", "Pandaemonium", "Titan",
        // Meteor (Check duplicates/moves - based on user image)
        "Belias", "Mandragora", "Shinryu", 
        // Note: Ramuh, Unicorn, Valefor, Yojimbo, Zeromus are listed in multiple spots in comments above but 
        // strictly: 
        // Elemental: Aegis, Atomos, Carbuncle, Garuda, Gungnir, Kujata, Tonberry, Typhon
        // Gaia: Alexander, Bahamut, Durandal, Fenrir, Ifrit, Ridill, Tiamat, Ultima
        // Mana: Anima, Asura, Chocobo, Hades, Ixion, Masamune, Pandaemonium, Titan
        // Meteor: Belias, Mandragora, Ramuh, Shinryu, Unicorn, Valefor, Yojimbo, Zeromus
    ];
    // Corrected lists based on 7.0 DC Travel/Meteor shuffle:
    // Elemental: Aegis, Atomos, Carbuncle, Garuda, Gungnir, Kujata, Tonberry, Typhon
    // Gaia: Alexander, Bahamut, Durandal, Fenrir, Ifrit, Ridill, Tiamat, Ultima
    // Mana: Anima, Asura, Chocobo, Hades, Ixion, Masamune, Pandaemonium, Titan
    // Meteor: Belias, Mandragora, Ramuh, Shinryu, Unicorn, Valefor, Yojimbo, Zeromus
    
    // Flattened verify check:
    let jp_servers_flat = [
        "Aegis", "Atomos", "Carbuncle", "Garuda", "Gungnir", "Kujata", "Tonberry", "Typhon",
        "Alexander", "Bahamut", "Durandal", "Fenrir", "Ifrit", "Ridill", "Tiamat", "Ultima",
        "Anima", "Asura", "Chocobo", "Hades", "Ixion", "Masamune", "Pandaemonium", "Titan",
        "Belias", "Mandragora", "Ramuh", "Shinryu", "Unicorn", "Valefor", "Yojimbo", "Zeromus"
    ];

    // NA (Aether, Primal, Crystal, Dynamis)
    let na_servers = [
        // Aether
        "Adamantoise", "Cactuar", "Faerie", "Gilgamesh", "Jenova", "Midgardsormr", "Sargatanas", "Siren",
        // Primal
        "Behemoth", "Excalibur", "Exodus", "Famfrit", "Hyperion", "Lamia", "Leviathan", "Ultros",
        // Crystal
        "Balmung", "Brynhildr", "Coeurl", "Diabolos", "Goblin", "Malboro", "Mateus", "Zalera",
        // Dynamis
        "Halicarnassus", "Maduin", "Marilith", "Seraph", "Cuchulainn", "Golem", "Kraken", "Rafflesia",
    ];

    // EU (Chaos, Light) - Shadow removed
    let eu_servers = [
        // Chaos
        "Cerberus", "Louisoix", "Moogle", "Omega", "Phantom", "Ragnarok", "Sagittarius", "Spriggan",
        // Light
        "Alpha", "Lich", "Odin", "Phoenix", "Raiden", "Shiva", "Twintania", "Zodiark",
    ];

    // OCE (Materia)
    let oce_servers = ["Bismarck", "Ravana", "Sephirot", "Sophia", "Zurvan"];

    // Normalize input
    let s = server.trim();

    if jp_servers_flat.iter().any(|name| name.eq_ignore_ascii_case(s)) {
        "JP"
    } else if na_servers.iter().any(|name| name.eq_ignore_ascii_case(s)) {
        "NA"
    } else if eu_servers.iter().any(|name| name.eq_ignore_ascii_case(s)) {
        "EU"
    } else if oce_servers.iter().any(|name| name.eq_ignore_ascii_case(s)) {
        "OC" 
    } else {
        "NA" // Default fallback
    }
}
