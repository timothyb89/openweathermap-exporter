use std::{sync::{Arc, RwLock}, thread, str::FromStr, time::Duration};

use anyhow::{anyhow, Result};
use reqwest::blocking::Client;
use structopt::StructOpt;

use serde::{Serialize, Deserialize};
use serde_json::json;
use warp::Filter;

const OWM_API_ENDPOINT: &str = "https://api.openweathermap.org/data/2.5/weather";

#[derive(Debug, StructOpt)]
#[structopt(name = "openweathermap-exporter")]
struct Options {
  /// comma-separated lat/lon coords, e.g. 123.0,456.0
  coords: Coordinates,

  /// openweathermap api key
  #[structopt(long, short, env = "OWM_API_KEY")]
  api_key: String,

  /// refresh interval in seconds
  #[structopt(long, short, default_value = "60.0")]
  interval: f32,

  /// interval to wait if the previous request failed
  #[structopt(long, short, default_value = "180.0")]
  backoff_interval: f32,

  /// port for the http server
  #[structopt(long, short, default_value = "8081")]
  port: u16
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Coordinates {
  lat: f32,
  lon: f32
}

impl FromStr for Coordinates {
  type Err = anyhow::Error;
  
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    let mut iter = s.splitn(2, ',');
    let lat = iter.next()
      .and_then(|s| s.parse::<f32>().ok())
      .ok_or_else(|| anyhow!("invalid lat"))?;
    let lon = iter.next()
      .and_then(|s| s.parse::<f32>().ok())
      .ok_or_else(|| anyhow!("invalid lon"))?;

    Ok(Coordinates { lat, lon })
  }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReportCondition {
  id: u32,
  main: String,
  description: String,
  icon: String
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReportMain {
  temp: f32,
  feels_like: f32,
  temp_min: f32,
  temp_max: f32,
  pressure: f32,
  humidity: f32
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReportWind {
  pub speed: f32,
  pub deg: u32
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct ReportRain {
  pub volume_1h: Option<f32>,
  pub volume_3h: Option<f32>
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct ReportSnow {
  pub volume_1h: Option<f32>,
  pub volume_3h: Option<f32>
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReportClouds {
  all: u32
}

#[derive(Debug, Serialize, Deserialize)]
struct Report {
  coord: Coordinates,
  weather: Vec<ReportCondition>,
  main: ReportMain,

  wind: ReportWind,

  #[serde(default)]
  rain: ReportRain,

  #[serde(default)]
  snow: ReportSnow,
  clouds: ReportClouds
}

fn report_thread(report_lock: Arc<RwLock<Option<Report>>>, opts: Options) {
  thread::spawn(move || {
    let client = Client::new();

    loop {
      let response = client.get(OWM_API_ENDPOINT)
        .query(&[
          ("lat", &opts.coords.lat.to_string()),
          ("lon", &opts.coords.lon.to_string()),
          ("appid", &opts.api_key)
        ])
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.json::<Report>());

      let report = match response {
        Ok(response) => response,
        Err(e) => {
          eprintln!("owm api error: {:?}", e);
          thread::sleep(Duration::from_secs_f32(opts.backoff_interval));
          continue;
        }
      };

      // TODO logging lib
      *report_lock.try_write().unwrap() = Some(report);

      thread::sleep(Duration::from_secs_f32(opts.interval));
    }
  });
}


#[tokio::main]
async fn main() {
  let opts = Options::from_args();
  let port = opts.port;

  let latest_report_lock = Arc::new(RwLock::new(None));
  report_thread(latest_report_lock.clone(), opts);

  let json_lock = Arc::clone(&latest_report_lock);
  let r_json = warp::path("json").map(move || {
    match *json_lock.read().unwrap() {
      Some(ref r) => warp::reply::json(&r),
      None => warp::reply::json(&json!(null))
    }
  });

  let metrics_lock = Arc::clone(&latest_report_lock);
  let r_metrics = warp::path("metrics").map(move || {
    match *metrics_lock.read().unwrap() {
      // TODO
      Some(ref r) => format!(
        ""
      ),
      None => format!("")
    }
  });

  let routes = warp::get().and(r_json).or(r_metrics);
  warp::serve(routes).run(([0, 0, 0, 0], port)).await;
}

