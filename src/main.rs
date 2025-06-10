use std::time::Duration;

use actix_cors::Cors;
use actix_web::{
    App,
    HttpResponse,
    HttpServer,
    Responder,
    get,
    http::header::{CacheControl, CacheDirective},
    web,
};
use metascraper::MetaScraper;
use moka::future::Cache;
use serde::{Deserialize, Serialize};

const MAX_AGE: u64 = 86400; // 1 day in seconds

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let host = std::env::var("HOST").unwrap_or_else(|_| "127.0.0.1".to_owned());
    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".to_owned());
    let port = port.parse::<u16>().expect("Invalid port number");
    println!("Server running at http://{}:{}", host, port);
    let app = HttpServer::new(|| {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10)).pool_max_idle_per_host(10)
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/137.0.0.0 Safari/537.36")
            .build().expect("Failed to create HTTP client");
        let cache: Cache<String, Result<MetaData, String>> =
            Cache::builder().time_to_live(Duration::new(MAX_AGE, 0)).build();
        App::new()
            .wrap(Cors::permissive())
            .app_data(web::Data::new(client))
            .app_data(web::Data::new(cache))
            .service(link_preview)
    });
    app.bind((host.as_str(), port))?.run().await
}

const MAX_SIZE: usize = 1024 * 1024; // 1MB limit

async fn fetch_text(reqwest: &reqwest::Client, url: &str) -> anyhow::Result<String> {
    let response = reqwest.get(url).send().await?;
    let (mut text, mut total_size) = (String::with_capacity(8192.min(MAX_SIZE)), 0);
    let mut response = response;
    while let Some(chunk) = response.chunk().await? {
        let chunk_len = chunk.len();
        if total_size + chunk_len <= MAX_SIZE {
            text.push_str(std::str::from_utf8(&chunk)?);
            total_size += chunk_len;
            continue;
        }
        let remaining = MAX_SIZE - total_size;
        if remaining == 0 {
            break;
        }
        let valid_end = std::str::from_utf8(&chunk[..remaining])
            .map(|_| remaining)
            .unwrap_or_else(|e| e.valid_up_to());
        if valid_end > 0 {
            text.push_str(std::str::from_utf8(&chunk[..valid_end])?);
        }
        break;
    }
    Ok(text)
}

async fn fetch_metadata(
    reqwest: &reqwest::Client,
    url: &str,
) -> anyhow::Result<metascraper::MetaData> {
    Ok(MetaScraper::parse(&fetch_text(reqwest, url).await?)?.metadata())
}

#[derive(Deserialize)]
struct LinkPreviewQuery {
    url: String,
}

#[derive(Serialize, Clone)]
pub struct Metatag {
    pub name: String,
    pub content: String,
}

impl From<metascraper::Metatag> for Metatag {
    fn from(metag: metascraper::Metatag) -> Self {
        Metatag { name: metag.name, content: metag.content }
    }
}

#[derive(Serialize, Clone)]
pub struct MetaData {
    pub title: Option<String>,
    pub description: Option<String>,
    pub canonical: Option<String>,
    pub language: Option<String>,
    pub rss: Option<String>,
    pub image: Option<String>,
    pub amp: Option<String>,
    pub author: Option<String>,
    pub date: Option<String>,
    pub metatags: Option<Vec<Metatag>>,
}

impl From<metascraper::MetaData> for MetaData {
    fn from(data: metascraper::MetaData) -> Self {
        let metatags =
            data.metatags.map(|tags| tags.into_iter().map(Metatag::from).collect());
        MetaData {
            title: data.title,
            description: data.description,
            canonical: data.canonical,
            language: data.language,
            rss: data.rss,
            image: data.image,
            amp: data.amp,
            author: data.author,
            date: data.date,
            metatags,
        }
    }
}

#[get("/link_preview")]
async fn link_preview(
    reqwest: web::Data<reqwest::Client>,
    cache: web::Data<Cache<String, Result<MetaData, String>>>,
    query: web::Query<LinkPreviewQuery>,
) -> impl Responder {
    let url = &query.url;
    let result = match cache.get(url).await {
        Some(result) => result,
        None => {
            let result = fetch_metadata(&reqwest, url).await;
            let result = result.map(MetaData::from).map_err(|e| e.to_string());
            cache.insert(url.clone(), result.clone()).await;
            result
        }
    };
    match result {
        Ok(metadata) => HttpResponse::Ok()
            .insert_header(CacheControl(vec![CacheDirective::MaxAge(MAX_AGE as u32)]))
            .json(metadata),
        Err(error) => HttpResponse::InternalServerError().body(error),
    }
}
