#[macro_use] extern crate log;

use std::str::FromStr;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;
use std::fmt;

use anyhow::{anyhow, Result};
use reqwest::blocking::Client;
use structopt::StructOpt;

use serde::{Serialize, Deserialize};
use serde_json::json;
use warp::Filter;

const OWM_API_ENDPOINT: &str = "https://api.openweathermap.org/data/2.5/weather";

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum Units {
  Kelvin,
  Imperial,
  Metric,
}

impl Units {
  fn api_param(&self) -> Option<&'static str> {
    match self {
      Units::Kelvin => None,
      Units::Metric => Some("metric"),
      Units::Imperial => Some("imperial")
    }
  }

  fn units_pressure(&self) -> &'static str {
    "hPa"
  }

  fn units_temp(&self) -> &'static str {
    match self {
      Units::Kelvin => "k",
      Units::Metric => "c",
      Units::Imperial => "f"
    }
  }

  fn units_speed(&self) -> &'static str {
    match self {
      Units::Kelvin | Units::Metric => "m/s",
      Units::Imperial => "mph"
    }
  }
}

impl fmt::Display for Units {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}", match self {
      Units::Kelvin => "kelvin",
      Units::Metric => "metric",
      Units::Imperial => "imperial"
    })
  }
}

impl FromStr for Units {
  type Err = anyhow::Error;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s.to_ascii_lowercase().as_str() {
      "kelvin" => Ok(Units::Kelvin),
      "metric" => Ok(Units::Metric),
      "imperial" => Ok(Units::Imperial),
      s => Err(anyhow!("invalid units type '{}', must be one of: kelvin, metric, imperial", s))
    }
  }
}

#[derive(Debug, Clone, StructOpt)]
#[structopt(name = "openweathermap-exporter")]
struct Options {
  /// comma-separated lat/lon coords, e.g. 123.0,456.0
  coords: Coordinates,

  /// unit type, one of: kelvin, metric, imperial
  #[structopt(long, short, default_value = "kelvin", env = "OWM_UNITS")]
  units: Units,

  /// openweathermap api key
  #[structopt(long, short, env = "OWM_API_KEY")]
  api_key: String,

  /// refresh interval in seconds
  #[structopt(long, short, default_value = "120.0", env = "OWM_INTERVAL")]
  interval: f32,

  /// interval to wait if the previous request failed
  #[structopt(long, short, default_value = "180.0", env = "OWM_BACKOFF_INTERVAL")]
  backoff_interval: f32,

  /// port for the http server
  #[structopt(long, short, default_value = "8081", env = "OWM_PORT")]
  port: u16,

  /// if set, adds a `location=$location` label to all exported metrics
  #[structopt(long, short)]
  location: Option<String>
}

#[derive(Debug, Serialize, Deserialize, Clone)]
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
  clouds: ReportClouds,

  /// visibility in meters (does not honor units param)
  visibility: Option<usize>
}

/// Custom Result-Option hybrid to expose errors from the reporting thread
enum MaybeReport {
  Ok(Report),
  Err(Option<u16>),
  None
}

fn report_thread(report_lock: Arc<RwLock<MaybeReport>>, opts: Options) {
  thread::spawn(move || {
    let client = Client::new();

    let mut query: Vec<(String, String)> = vec![
      ("lat".into(), opts.coords.lat.to_string()),
      ("lon".into(), opts.coords.lon.to_string()),
      ("appid".into(), opts.api_key)
    ];

    if let Some(unit) = opts.units.api_param() {
      query.push(("units".into(), unit.to_string()));
    }

    loop {
      let response = client.get(OWM_API_ENDPOINT)
        .query(&query)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.json::<Report>());

      let report = match response {
        Ok(response) => response,
        Err(e) => {
          error!("owm api error: {:?}", e);
          *report_lock.try_write().unwrap() = MaybeReport::Err(e.status().map(|s| s.as_u16()));

          thread::sleep(Duration::from_secs_f32(opts.backoff_interval));
          continue;
        }
      };

      info!("report: {:?}", &report.main);
      debug!("full report: {:#?}", &report);

      *report_lock.try_write().unwrap() = MaybeReport::Ok(report);

      thread::sleep(Duration::from_secs_f32(opts.interval));
    }
  });
}

/// formats a prometheus metric
fn export<F>(opts: &Options, name: &str, value: F, mut labels: Vec<(String, String)>) -> String
where
  F: Into<f64>
{
  let mut effective_labels = Vec::new();
  effective_labels.append(&mut labels);

  if let Some(location) = &opts.location {
    effective_labels.push(("location".to_string(), location.clone()));
  }

  let mut s = String::new();
  s.push_str(name);

  if !effective_labels.is_empty() {
    s.push('{');
    for (i, (k, v)) in effective_labels.iter().enumerate() {
      if i > 0 {
        s.push(',');
      }

      s.push_str(k);
      s.push_str("=\"");
      s.push_str(v);
      s.push('"');
    }
    s.push('}');
  }

  s.push(' ');
  s.push_str(&value.into().to_string());
  s.push('\n');

  s
}

macro_rules! export {
  ($dest:ident, $opts:ident, $name:expr, $value:expr, $($label_key:ident = $label_value:expr),*) => {
    let mut labels: Vec<(String, String)> = Vec::new();
    $(labels.push((stringify!($label_key).to_string(), $label_value.to_string()));)*

    $dest.push_str(&export($opts, $name, $value, labels));
  };

  ($dest:ident, $opts:ident, $name:expr, $value:expr) => {
    $dest.push_str(&export($opts, $name, $value, vec![]));
  };
}

fn export_report(report: &MaybeReport, opts: &Options) -> String {
  let mut s = String::new();

  let report = match report {
    MaybeReport::Ok(report) => report,
    MaybeReport::None => return s,
    MaybeReport::Err(code) => {
      export!(s, opts, "owm_error", 1);
      if let Some(code) = code {
        export!(s, opts, "owm_error", 1, code = code.to_string());
      }

      return s;
    },
  };

  export!(s, opts, "owm_error", 0);

  export!(s, opts, "owm_temp", report.main.temp, unit = opts.units.units_temp());
  export!(s, opts, "owm_temp_min", report.main.temp_min, unit = opts.units.units_temp());
  export!(s, opts, "owm_temp_max", report.main.temp_max, unit = opts.units.units_temp());
  export!(s, opts, "owm_feels_like", report.main.feels_like, unit = opts.units.units_temp());
  export!(s, opts, "owm_humidity", report.main.humidity, unit = "percent");
  export!(s, opts, "owm_pressure", report.main.pressure, unit = opts.units.units_pressure());

  export!(s, opts, "owm_clouds_all", report.clouds.all, unit = "percent");

  if let Some(volume) = report.rain.volume_1h {
    export!(s, opts, "owm_rain_volume", volume, period = "1h", unit = "mm");
  }

  if let Some(volume) = report.rain.volume_3h {
    export!(s, opts, "owm_rain_volume", volume, period = "3h", unit = "mm");
  }

  if let Some(volume) = report.snow.volume_1h {
    export!(s, opts, "owm_snow_volume", volume, period = "1h", unit = "mm");
  }

  if let Some(volume) = report.snow.volume_3h {
    export!(s, opts, "owm_snow_volume", volume, period = "3h", unit = "mm");
  }

  export!(s, opts, "owm_wind_direction", report.wind.deg, unit = "degrees");
  export!(s, opts, "owm_wind_speed", report.wind.speed, unit = opts.units.units_speed());

  for condition in &report.weather {
    export!(s, opts, "owm_condition", 1, kind = condition.description);
  }

  if let Some(visibility) = report.visibility {
    export!(s, opts, "owm_visiblity", visibility as f64, unit = "meters");
  }

  s
}

#[tokio::main]
async fn main() {
  env_logger::init();

  let opts = Options::from_args();
  let port = opts.port;

  let latest_report_lock = Arc::new(RwLock::new(MaybeReport::None));
  report_thread(latest_report_lock.clone(), opts.clone());

  let json_lock = Arc::clone(&latest_report_lock);
  let r_json = warp::path("json").map(move || {
    match *json_lock.read().unwrap() {
      MaybeReport::Ok(ref r) => warp::reply::json(&r),
      MaybeReport::None => warp::reply::json(&json!(null)),
      MaybeReport::Err(e) => warp::reply::json(&json!({
        "error": e
      }))
    }
  });

  let metrics_lock = Arc::clone(&latest_report_lock);
  let r_metrics = warp::path("metrics").map(move || {
    export_report(&*metrics_lock.read().unwrap(), &opts)
  });

  let routes = warp::get().and(r_json).or(r_metrics);
  warp::serve(routes).run(([0, 0, 0, 0], port)).await;
}

