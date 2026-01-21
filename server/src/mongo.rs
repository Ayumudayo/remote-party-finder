use anyhow::Context;
use crate::listing::PartyFinderListing;
use crate::listing_container::{ListingContainer, QueriedListing};
use chrono::{TimeDelta, Utc};
use futures_util::StreamExt;
use mongodb::bson::doc;
use mongodb::results::UpdateResult;
use mongodb::Collection;
use mongodb::options::UpdateOptions;

pub async fn get_current_listings(
    collection: Collection<ListingContainer>,
) -> anyhow::Result<Vec<QueriedListing>> {
    let two_hours_ago = Utc::now() - TimeDelta::try_hours(2).unwrap();
    let cursor = collection
        .aggregate(
            [
                // don't ask me why, but mongo shits itself unless you provide a hard date
                // doc! {
                //     "$match": {
                //         "created_at": {
                //             "$gte": {
                //                 "$dateSubtract": {
                //                     "startDate": "$$NOW",
                //                     "unit": "hour",
                //                     "amount": 2,
                //                 },
                //             },
                //         },
                //     }
                // },
                doc! {
                    "$match": {
                        "updated_at": { "$gte": two_hours_ago },
                    }
                },
                doc! {
                    "$match": {
                        // filter private pfs
                        "listing.search_area": { "$bitsAllClear": 2 },
                    }
                },
                doc! {
                    "$set": {
                        "time_left": {
                            "$divide": [
                                {
                                    "$subtract": [
                                        { "$multiply": ["$listing.seconds_remaining", 1000] },
                                        { "$subtract": ["$$NOW", "$updated_at"] },
                                    ]
                                },
                                1000,
                            ]
                        },
                        "updated_minute": {
                            "$dateTrunc": {
                                "date": "$updated_at",
                                "unit": "minute",
                                "binSize": 5,
                            },
                        },
                    }
                },
                doc! {
                    "$match": {
                        "time_left": { "$gte": 0 },
                    }
                },
            ],
            None,
        )
        .await?;

    let collect = cursor
        .filter_map(async |res| {
            res.ok()
                .and_then(|doc| mongodb::bson::from_document(doc).ok())
        })
        .collect::<Vec<_>>()
        .await;

    Ok(collect)
}

pub async fn insert_listing(
    collection: Collection<ListingContainer>,
    listing: &PartyFinderListing,
) -> anyhow::Result<UpdateResult> {
    if listing.created_world >= 1_000
        || listing.home_world >= 1_000
        || listing.current_world >= 1_000
    {
        anyhow::bail!("invalid listing");
    }

    let opts = UpdateOptions::builder().upsert(true).build();
    let bson_value = mongodb::bson::to_bson(&listing)?;
    let now = Utc::now();
    collection
        .update_one(
            doc! {
                "listing.id": listing.id,
                "listing.last_server_restart": listing.last_server_restart,
                "listing.created_world": listing.created_world as u32,
            },
            doc! {
                "$currentDate": {
                    "updated_at": true,
                },
                "$set": {
                    "listing": bson_value,
                },
                "$setOnInsert": {
                    "created_at": now,
                },
            },
            opts,
        )
        .await
        .context("could not insert record")
}

/// 플레이어 정보를 upsert (있으면 업데이트, 없으면 삽입)
pub async fn upsert_players(
    collection: Collection<crate::player::Player>,
    players: &[crate::player::UploadablePlayer],
) -> anyhow::Result<usize> {
    let mut successful = 0;
    let now = Utc::now();

    for player in players {
        if player.content_id == 0 || player.name.is_empty() || player.home_world >= 1_000 {
            continue;
        }

        let opts = UpdateOptions::builder().upsert(true).build();
        let result = collection
            .update_one(
                doc! { "content_id": player.content_id as i64 },
                doc! {
                    "$set": {
                        "name": &player.name,
                        "home_world": player.home_world as u32,
                        "last_seen": now,
                    },
                    "$inc": { "seen_count": 1 },
                    "$setOnInsert": {
                        "content_id": player.content_id as i64,
                    },
                },
                opts,
            )
            .await;

        if result.is_ok() {
            successful += 1;
        }
    }

    Ok(successful)
}

/// ContentID 목록으로 플레이어 정보 조회
pub async fn get_players_by_content_ids(
    collection: Collection<crate::player::Player>,
    content_ids: &[u64],
) -> anyhow::Result<Vec<crate::player::Player>> {
    let ids: Vec<i64> = content_ids.iter().map(|&id| id as i64).collect();
    
    let cursor = collection
        .find(doc! { "content_id": { "$in": ids } }, None)
        .await?;

    let players = cursor
        .filter_map(async |res| {
            match res {
                Ok(p) => Some(p),
                Err(e) => {
                    eprintln!("Error reading player: {:?}", e);
                    None
                }
            }
        })
        .collect::<Vec<_>>()
        .await;

    Ok(players)
}
