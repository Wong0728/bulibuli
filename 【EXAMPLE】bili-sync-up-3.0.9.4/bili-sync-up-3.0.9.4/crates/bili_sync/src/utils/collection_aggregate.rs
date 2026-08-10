use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use anyhow::Result;
use once_cell::sync::Lazy;

use crate::api::response::UserCollection;
use crate::bilibili::BiliClient;

const COLLECTION_AGGREGATE_CACHE_TTL: Duration = Duration::from_secs(10 * 60);
const COLLECTION_AGGREGATE_PAGE_SIZE: u32 = 20;

#[derive(Clone)]
struct CachedCollectionAggregate {
    fetched_at: Instant,
    season_numbers: HashMap<String, i32>,
}

static COLLECTION_AGGREGATE_CACHE: Lazy<RwLock<HashMap<i64, CachedCollectionAggregate>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

pub fn collection_type_name_from_db(collection_type: i32) -> &'static str {
    if collection_type == 1 {
        "series"
    } else {
        "season"
    }
}

pub fn build_bili_client_from_config() -> BiliClient {
    let config = crate::config::reload_config();
    let credential = config.credential.load();
    let cookie = credential
        .as_ref()
        .map(|cred| {
            format!(
                "SESSDATA={};bili_jct={};buvid3={};DedeUserID={};ac_time_value={}",
                cred.sessdata, cred.bili_jct, cred.buvid3, cred.dedeuserid, cred.ac_time_value
            )
        })
        .unwrap_or_default();
    BiliClient::new(cookie)
}

fn build_collection_cache_key(sid: &str, collection_type: &str) -> String {
    format!("{}:{}", collection_type, sid)
}

fn build_collection_season_number_map(collections: &[UserCollection]) -> HashMap<String, i32> {
    collections
        .iter()
        .enumerate()
        .map(|(index, item)| {
            (
                build_collection_cache_key(&item.sid, &item.collection_type),
                index as i32 + 1,
            )
        })
        .collect()
}

fn get_cached_collection_season_number(up_mid: i64, sid: &str, collection_type: &str) -> Option<i32> {
    let cache = COLLECTION_AGGREGATE_CACHE.read().ok()?;
    let cached = cache.get(&up_mid)?;
    if cached.fetched_at.elapsed() > COLLECTION_AGGREGATE_CACHE_TTL {
        return None;
    }
    cached
        .season_numbers
        .get(&build_collection_cache_key(sid, collection_type))
        .copied()
}

pub fn cache_user_collections(up_mid: i64, collections: &[UserCollection]) {
    if collections.is_empty() {
        return;
    }
    let season_numbers = build_collection_season_number_map(collections);
    if let Ok(mut cache) = COLLECTION_AGGREGATE_CACHE.write() {
        cache.insert(
            up_mid,
            CachedCollectionAggregate {
                fetched_at: Instant::now(),
                season_numbers,
            },
        );
    }
}

pub async fn fetch_absolute_collection_season_number(
    up_mid: i64,
    collection_sid: i64,
    collection_type: i32,
) -> Result<Option<i32>> {
    let target_sid = collection_sid.to_string();
    let target_type = collection_type_name_from_db(collection_type);

    if let Some(cached) = get_cached_collection_season_number(up_mid, &target_sid, target_type) {
        return Ok(Some(cached));
    }

    let bili_client = build_bili_client_from_config();
    let response = bili_client
        .get_user_collections(up_mid, 1, COLLECTION_AGGREGATE_PAGE_SIZE)
        .await?;

    cache_user_collections(up_mid, &response.collections);

    Ok(build_collection_season_number_map(&response.collections)
        .get(&build_collection_cache_key(&target_sid, target_type))
        .copied())
}
