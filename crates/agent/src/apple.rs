//! Apple catalog probes used to route a request to an account whose
//! storefront can actually serve the item.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use regex::Regex;
use reqwest::Client;

const WEB_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";
const ANDROID_UA: &str = "Music/5.7 Android/10 model/Pixel6GR1YH build/1234 (dt:66)";
const DAY: Duration = Duration::from_secs(24 * 3600);
pub const DEFAULT_LYRICS_SCRIPT: &str = "en-Latn";

/// Small TTL map with a hard size cap (oldest entries are dropped first).
pub struct TtlCache<K, V> {
    map: Mutex<HashMap<K, (V, Instant)>>,
    ttl: Duration,
    cap: usize,
}

impl<K: Eq + Hash + Clone, V: Clone> TtlCache<K, V> {
    pub fn new(ttl: Duration, cap: usize) -> Self {
        Self { map: Mutex::new(HashMap::new()), ttl, cap }
    }

    pub fn get(&self, k: &K) -> Option<V> {
        let map = self.map.lock().unwrap();
        map.get(k).filter(|(_, t)| t.elapsed() < self.ttl).map(|(v, _)| v.clone())
    }

    pub fn insert(&self, k: K, v: V) {
        let mut map = self.map.lock().unwrap();
        if map.len() >= self.cap {
            map.retain(|_, (_, t)| t.elapsed() < self.ttl);
            if map.len() >= self.cap {
                // Still full of live entries: drop the oldest tenth.
                let mut ages: Vec<_> = map.iter().map(|(k, (_, t))| (*t, k.clone())).collect();
                ages.sort_by_key(|(t, _)| *t);
                for (_, k) in ages.into_iter().take(self.cap / 10 + 1) {
                    map.remove(&k);
                }
            }
        }
        map.insert(k, (v, Instant::now()));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Song,
    MusicVideo,
}

pub struct AppleClient {
    http: Client,
    token: tokio::sync::Mutex<Option<(String, Instant)>>,
    last_failure: Mutex<Option<Instant>>,
    catalog: TtlCache<(Kind, String, String), bool>,
    lyrics: TtlCache<String, bool>,
    reach_url: Mutex<String>,
    storefronts: tokio::sync::Mutex<StorefrontCache>,
}

/// Storefront code -> language tags its store supports (`jp` -> `["ja", "en-US"]`).
pub type StorefrontLangs = HashMap<String, Vec<String>>;

#[derive(Default)]
struct StorefrontCache {
    langs: Option<Arc<StorefrontLangs>>,
    failed_at: Option<Instant>,
}

/// Comparable form of a language tag: `ja-JP` -> `ja`, `en-US` -> `en`,
/// `zh-Hant-TW` -> `zh-hant` (the script is kept for Chinese).
pub fn normalize_lang_tag(tag: &str) -> String {
    let lower = tag.trim().to_lowercase();
    for script in ["zh-hant", "zh-hans"] {
        if lower.starts_with(script) {
            return script.to_string();
        }
    }
    match lower.find('-') {
        Some(i) if i > 0 => lower[..i].to_string(),
        _ => lower,
    }
}

/// Whether `region`'s store lists a language matching `language`.
pub fn region_supports_lang(langs: &StorefrontLangs, region: &str, language: &str) -> bool {
    let want = normalize_lang_tag(language);
    !want.is_empty()
        && langs
            .get(&region.to_lowercase())
            .is_some_and(|tags| tags.iter().any(|t| normalize_lang_tag(t) == want))
}

/// Cache key of a lyrics availability probe. An empty script is the default
/// one, so `""` and `en-Latn` share an entry instead of probing twice.
fn lyrics_key(region: &str, language: &str, script: &str, adam_id: &str) -> String {
    let script = if script.is_empty() { DEFAULT_LYRICS_SCRIPT } else { script };
    format!("{region}/{language}/{script}/{adam_id}")
}

/// The FairPlay key server lite talks to; see [`AppleClient::reachable`].
const KEY_SERVER_URL: &str = "https://play.itunes.apple.com/WebObjects/MZPlay.woa/music/fps";

static INDEX_JS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/assets/index[~-][^/\x22']+\.js").unwrap());
static JWT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"eyJ[A-Za-z0-9_=-]+\.[A-Za-z0-9_=-]+\.[A-Za-z0-9_=-]+").unwrap());

impl AppleClient {
    pub fn new(proxy: Option<&str>) -> anyhow::Result<Self> {
        let mut b = Client::builder().timeout(Duration::from_secs(15)).user_agent(WEB_UA);
        if let Some(p) = proxy.filter(|p| !p.is_empty()) {
            let p = if p.contains("://") { p.to_string() } else { format!("http://{p}") };
            b = b.proxy(reqwest::Proxy::all(p)?);
        }
        Ok(Self {
            http: b.build()?,
            token: tokio::sync::Mutex::new(None),
            last_failure: Mutex::new(None),
            catalog: TtlCache::new(DAY, 200_000),
            lyrics: TtlCache::new(DAY, 50_000),
            reach_url: Mutex::new(KEY_SERVER_URL.to_string()),
            storefronts: tokio::sync::Mutex::new(StorefrontCache::default()),
        })
    }

    /// Which languages each storefront supports (Apple's `/v1/storefronts`),
    /// fetched once and kept: the list hardly ever changes. `None` while it
    /// cannot be fetched (retried after a minute).
    pub async fn storefront_langs(&self) -> Option<Arc<StorefrontLangs>> {
        let mut cache = self.storefronts.lock().await;
        if let Some(l) = &cache.langs {
            return Some(l.clone());
        }
        if cache.failed_at.is_some_and(|t| t.elapsed() < Duration::from_secs(60)) {
            return None;
        }
        match self.fetch_storefront_langs().await {
            Ok(m) => {
                let m = Arc::new(m);
                cache.langs = Some(m.clone());
                Some(m)
            }
            Err(e) => {
                tracing::warn!("failed to fetch storefront languages: {e}");
                cache.failed_at = Some(Instant::now());
                None
            }
        }
    }

    async fn fetch_storefront_langs(&self) -> anyhow::Result<StorefrontLangs> {
        let token = self.web_token().await?;
        let mut map = StorefrontLangs::new();
        let mut next = "/v1/storefronts".to_string();
        // Follow `next` in case the list is paged.
        for _ in 0..10 {
            let resp = self
                .http
                .get(format!("https://api.music.apple.com{next}"))
                .bearer_auth(&token)
                .header("Origin", "https://music.apple.com")
                .send()
                .await?;
            if !resp.status().is_success() {
                anyhow::bail!("storefronts API returned {}", resp.status());
            }
            let v: serde_json::Value = resp.json().await?;
            for sf in v.get("data").and_then(|d| d.as_array()).into_iter().flatten() {
                let tags: Vec<String> = sf
                    .pointer("/attributes/supportedLanguageTags")
                    .and_then(|t| t.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|t| t.as_str().map(str::to_string))
                    .collect();
                if let (Some(id), false) = (sf.get("id").and_then(|i| i.as_str()), tags.is_empty()) {
                    map.insert(id.to_lowercase(), tags);
                }
            }
            match v.get("next").and_then(|n| n.as_str()) {
                Some(n) => next = n.to_string(),
                None => break,
            }
        }
        if map.is_empty() {
            anyhow::bail!("storefronts API returned no data");
        }
        Ok(map)
    }

    /// Whether Apple's FairPlay key server answers at all, through the same
    /// proxy as lite. Any HTTP status counts (the bare endpoint rejects a
    /// plain GET); only a transport failure (DNS, connect, TLS, proxy,
    /// timeout) does not. This is what tells "the network is down" apart from
    /// "lite is broken" when a key request fails.
    pub async fn reachable(&self) -> bool {
        let url = self.reach_url.lock().unwrap().clone();
        self.http.get(url).timeout(Duration::from_secs(10)).send().await.is_ok()
    }

    #[cfg(test)]
    pub(crate) fn set_reach_url(&self, url: &str) {
        *self.reach_url.lock().unwrap() = url.to_string();
    }

    /// The AMPWebPlay bearer token embedded in music.apple.com's JS bundle.
    pub async fn web_token(&self) -> anyhow::Result<String> {
        let mut guard = self.token.lock().await;
        if let Some((t, at)) = guard.as_ref() {
            if at.elapsed() < DAY {
                return Ok(t.clone());
            }
        }
        if let Some(at) = *self.last_failure.lock().unwrap() {
            if at.elapsed() < Duration::from_secs(60) {
                anyhow::bail!("web token unavailable (recent scrape failed)");
            }
        }
        match self.scrape_token().await {
            Ok(t) => {
                *guard = Some((t.clone(), Instant::now()));
                Ok(t)
            }
            Err(e) => {
                *self.last_failure.lock().unwrap() = Some(Instant::now());
                // Keep serving a stale token rather than nothing.
                if let Some((t, _)) = guard.as_ref() {
                    return Ok(t.clone());
                }
                Err(e)
            }
        }
    }

    async fn scrape_token(&self) -> anyhow::Result<String> {
        let home = self.http.get("https://music.apple.com").send().await?.text().await?;
        let js = INDEX_JS.find(&home).ok_or_else(|| anyhow::anyhow!("index js not found"))?.as_str();
        let body = self.http.get(format!("https://music.apple.com{js}")).send().await?.text().await?;
        let token = JWT.find(&body).ok_or_else(|| anyhow::anyhow!("token not found in {js}"))?;
        Ok(token.as_str().to_string())
    }

    /// Whether `adam_id` exists in the `region` catalog.
    pub async fn available(&self, kind: Kind, region: &str, adam_id: &str) -> anyhow::Result<bool> {
        if adam_id == "0" {
            return Ok(true);
        }
        let key = (kind, region.to_string(), adam_id.to_string());
        if let Some(v) = self.catalog.get(&key) {
            return Ok(v);
        }
        let path = match kind {
            Kind::Song => "songs",
            Kind::MusicVideo => "music-videos",
        };
        let token = self.web_token().await?;
        let resp = self
            .http
            .get(format!("https://amp-api.music.apple.com/v1/catalog/{region}/{path}/{adam_id}"))
            .bearer_auth(token)
            .header("Origin", "https://music.apple.com")
            .send()
            .await?;
        let status = resp.status();
        if status.is_server_error() || status.as_u16() == 401 || status.as_u16() == 429 {
            anyhow::bail!("catalog probe returned {status}");
        }
        let v: serde_json::Value = resp.json().await.unwrap_or_default();
        let ok = v.get("errors").is_none() && v.get("data").and_then(|d| d.as_array()).is_some_and(|d| !d.is_empty());
        self.catalog.insert(key, ok);
        Ok(ok)
    }

    /// Whether the region has (syllable) lyrics for the song in `language`.
    pub async fn has_lyrics(&self, region: &str, adam_id: &str, language: &str, script: &str, music_token: &str) -> bool {
        let script = if script.is_empty() { DEFAULT_LYRICS_SCRIPT } else { script };
        let key = lyrics_key(region, language, script, adam_id);
        if let Some(v) = self.lyrics.get(&key) {
            return v;
        }
        let Ok(token) = self.web_token().await else {
            return false;
        };
        let url = format!(
            "https://amp-api.music.apple.com/v1/catalog/{region}/songs/{adam_id}/syllable-lyrics?l[lyrics]={language}&extend=ttmlLocalizations&l[script]={script}"
        );
        let ok = match self
            .http
            .head(url)
            .bearer_auth(token)
            .header("User-Agent", ANDROID_UA)
            .header("media-user-token", music_token)
            .header("Origin", "https://music.apple.com")
            .send()
            .await
        {
            Ok(r) => r.status().is_success(),
            // Transport errors are not cached: try again next time.
            Err(_) => return false,
        };
        self.lyrics.insert(key, ok);
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ported from wrapper-manager's storefront_test.go.
    #[test]
    fn normalizes_language_tags() {
        for (tag, want) in [
            ("ja", "ja"), ("ja-JP", "ja"), ("ko", "ko"),
            ("zh-Hant-TW", "zh-hant"), ("zh-Hans-CN", "zh-hans"), ("zh-Hant", "zh-hant"),
            ("en-US", "en"), ("en-GB", "en"), ("fr-FR", "fr"),
            ("  pt-BR  ", "pt"), ("", ""), ("XX", "xx"),
        ] {
            assert_eq!(normalize_lang_tag(tag), want, "{tag:?}");
        }
    }

    #[test]
    fn regions_support_languages() {
        let m: StorefrontLangs = [
            ("jp", vec!["ja", "en-US"]),
            ("kr", vec!["ko", "en-GB"]),
            ("cn", vec!["zh-Hans-CN", "en-GB"]),
            ("tw", vec!["zh-Hant-TW", "en-GB"]),
            ("th", vec!["en-GB", "th"]),
            ("us", vec!["en-US", "es-MX", "ko", "zh-Hant-TW"]),
        ]
        .into_iter()
        .map(|(r, t)| (r.to_string(), t.into_iter().map(String::from).collect()))
        .collect();
        for (region, lang, want) in [
            ("jp", "ja", true), ("jp", "ja-JP", true), ("kr", "ko", true), ("cn", "zh-Hans", true),
            ("cn", "zh-Hans-CN", true), ("tw", "zh-Hant", true), ("us", "ko", true), ("th", "th", true),
            ("jp", "ko", false), ("kr", "ja", false), ("th", "ko", false),
            ("jp", "en", true), // en-US normalized => en matches
            ("JP", "ja", true), ("xx", "ja", false), ("jp", "", false),
        ] {
            assert_eq!(region_supports_lang(&m, region, lang), want, "{region} {lang}");
        }
    }

    #[test]
    fn lyrics_probe_cache_key_ignores_empty_script() {
        assert_eq!(lyrics_key("us", "en", "", "1"), lyrics_key("us", "en", "en-Latn", "1"));
        assert_ne!(lyrics_key("us", "en", "ja-Latn", "1"), lyrics_key("us", "en", "en-Latn", "1"));
        assert_ne!(lyrics_key("us", "en", "", "1"), lyrics_key("jp", "en", "", "1"));
    }

    #[test]
    fn ttl_cache_caps_size() {
        let c = TtlCache::new(Duration::from_secs(60), 10);
        for i in 0..25 {
            c.insert(i, i);
        }
        assert!(c.map.lock().unwrap().len() <= 10);
        assert_eq!(c.get(&24), Some(24));
    }

    #[test]
    fn regexes_match_bundle_shapes() {
        assert!(INDEX_JS.is_match(r#"<script src="/assets/index~a1b2c3.js">"#));
        assert!(INDEX_JS.is_match(r#"<script src="/assets/index-DyQ9.js">"#));
        assert!(JWT.is_match("x=\"eyJhbGciOi.eyJpc3Mi.sig-_=\""));
    }
}
