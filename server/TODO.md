# Server Code Improvement TODO

> 분석일: 2026-01-24

## High Priority

### 1. ~~N+1 Query in background.rs~~ ✅
- [x] `fetch_parses_task`에서 Zone 캐시 확인 시 개별 DB 조회 → 배치 조회로 변경
- 위치: `background.rs` L126-144
- 기존 `get_zone_caches()` 함수 활용 가능 (`mongo.rs` L265-294)

### 2. ~~Duplicate Parse Lookup Logic in handlers.rs~~ ✅
- [x] 멤버/파티장 Parse 조회 로직 헬퍼 함수로 추출
- 위치: `handlers.rs` L100-128 (멤버), L149-171 (파티장)

## Medium Priority

### 3. ~~Redundant Sort Operations in handlers.rs~~ ✅
- [x] 3번 정렬 → 단일 정렬로 통합
- 위치: `handlers.rs` L25-35

### 4. ~~ParseDisplay Struct Extraction~~ ✅
- [x] `RenderableListing`, `RenderableMember`의 Parse 필드 별도 구조체로 추출
- 위치: `template/listings.rs`

## Low Priority (Optional)

### 5. ~~Module Structure Reorganization~~ ✅
- [x] `fflogs/` 디렉토리 생성
  - `client.rs` ← 기존 `fflogs.rs`
  - `mapping.rs` ← 기존 `fflogs_mapping.rs`  
  - `cache.rs` ← Parse 캐시 타입 (`mongo.rs`에서 분리)
- 현재 기능 정상 동작 확인
