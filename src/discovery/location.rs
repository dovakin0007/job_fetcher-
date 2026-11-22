use crate::models::SearchConfig;
use serde::Deserialize;

#[derive(Debug, Clone, Copy)]
pub struct GeoFix {
    pub latitude: f64,
    pub longitude: f64,
}

#[derive(Debug, Clone)]
struct Place {
    name: &'static str,
    aliases: &'static [&'static str],
    country: &'static str,
    lat: f64,
    lon: f64,
}

struct Detected {
    city: String,
    region: String,
    country: String,
    latitude: f64,
    longitude: f64,
    source: &'static str,
}

const PLACES: &[Place] = &[
    Place {
        name: "Chennai",
        aliases: &["madras"],
        country: "India",
        lat: 13.0827,
        lon: 80.2707,
    },
    Place {
        name: "Coimbatore",
        aliases: &[],
        country: "India",
        lat: 11.0168,
        lon: 76.9558,
    },
    Place {
        name: "Bengaluru",
        aliases: &["bangalore"],
        country: "India",
        lat: 12.9716,
        lon: 77.5946,
    },
    Place {
        name: "Hyderabad",
        aliases: &[],
        country: "India",
        lat: 17.3850,
        lon: 78.4867,
    },
    Place {
        name: "Mumbai",
        aliases: &["bombay"],
        country: "India",
        lat: 19.0760,
        lon: 72.8777,
    },
    Place {
        name: "Pune",
        aliases: &[],
        country: "India",
        lat: 18.5204,
        lon: 73.8567,
    },
    Place {
        name: "Delhi",
        aliases: &["new delhi"],
        country: "India",
        lat: 28.6139,
        lon: 77.2090,
    },
    Place {
        name: "Gurugram",
        aliases: &["gurgaon"],
        country: "India",
        lat: 28.4595,
        lon: 77.0266,
    },
    Place {
        name: "Noida",
        aliases: &[],
        country: "India",
        lat: 28.5355,
        lon: 77.3910,
    },
    Place {
        name: "Kolkata",
        aliases: &["calcutta"],
        country: "India",
        lat: 22.5726,
        lon: 88.3639,
    },
    Place {
        name: "Ahmedabad",
        aliases: &[],
        country: "India",
        lat: 23.0225,
        lon: 72.5714,
    },
    Place {
        name: "Kochi",
        aliases: &["cochin"],
        country: "India",
        lat: 9.9312,
        lon: 76.2673,
    },
    Place {
        name: "Jaipur",
        aliases: &[],
        country: "India",
        lat: 26.9124,
        lon: 75.7873,
    },
    Place {
        name: "Chandigarh",
        aliases: &[],
        country: "India",
        lat: 30.7333,
        lon: 76.7794,
    },
    Place {
        name: "Indore",
        aliases: &[],
        country: "India",
        lat: 22.7196,
        lon: 75.8577,
    },
    Place {
        name: "Lucknow",
        aliases: &[],
        country: "India",
        lat: 26.8467,
        lon: 80.9462,
    },
    Place {
        name: "Thiruvananthapuram",
        aliases: &["trivandrum"],
        country: "India",
        lat: 8.5241,
        lon: 76.9366,
    },
    Place {
        name: "Berlin",
        aliases: &[],
        country: "Germany",
        lat: 52.5200,
        lon: 13.4050,
    },
    Place {
        name: "London",
        aliases: &[],
        country: "United Kingdom",
        lat: 51.5074,
        lon: -0.1278,
    },
    Place {
        name: "New York",
        aliases: &["nyc"],
        country: "United States",
        lat: 40.7128,
        lon: -74.0060,
    },
    Place {
        name: "San Francisco",
        aliases: &[],
        country: "United States",
        lat: 37.7749,
        lon: -122.4194,
    },
    Place {
        name: "Toronto",
        aliases: &[],
        country: "Canada",
        lat: 43.6532,
        lon: -79.3832,
    },
    Place {
        name: "Singapore",
        aliases: &[],
        country: "Singapore",
        lat: 1.3521,
        lon: 103.8198,
    },
    Place {
        name: "Sydney",
        aliases: &[],
        country: "Australia",
        lat: -33.8688,
        lon: 151.2093,
    },
];

/// Detect where the user is, then point the saved search at that city and its neighbours.
/// Browser coordinates win. The scheduled run has none, so it uses the public-IP city and drops the IP.
pub async fn refresh(config: &SearchConfig, fix: Option<GeoFix>) -> anyhow::Result<SearchConfig> {
    let detected = detect(fix).await?;
    let mut next = config.clone();
    next.detected_city = detected.city.clone();
    next.detected_region = detected.region.clone();
    next.detected_country = detected.country.clone();
    next.location_source = detected.source.to_string();
    let (anchor, lat, lon, country) = if next.follow_location || next.location.trim().is_empty() {
        next.location = detected.city.clone();
        if !detected.country.is_empty() {
            next.target_country = detected.country.clone();
        }
        (
            detected.city,
            detected.latitude,
            detected.longitude,
            detected.country,
        )
    } else if let Some(place) = find_place(&next.location) {
        next.target_country = place.country.to_string();
        (
            place.name.to_string(),
            place.lat,
            place.lon,
            place.country.to_string(),
        )
    } else {
        (
            next.location.clone(),
            detected.latitude,
            detected.longitude,
            next.target_country.clone(),
        )
    };
    next.latitude = Some(lat);
    next.longitude = Some(lon);
    next.nearby = nearby_names(lat, lon, &country, &anchor, 2);
    Ok(next.normalized())
}

/// Recompute neighbours after the user edits the city, without calling a location service.
pub fn retarget(config: SearchConfig) -> SearchConfig {
    let mut config = config.normalized();
    if let Some(place) = find_place(&config.location) {
        config.target_country = place.country.to_string();
        config.latitude = Some(place.lat);
        config.longitude = Some(place.lon);
        config.nearby = nearby_names(place.lat, place.lon, place.country, place.name, 2);
    }
    config
}

async fn detect(fix: Option<GeoFix>) -> anyhow::Result<Detected> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .user_agent("rust-job-agent")
        .build()?;
    if let Some(fix) = fix {
        if let Ok(place) = reverse_nominatim(&client, fix).await {
            return Ok(place);
        }
        if let Ok(mut place) = ip_api(&client).await {
            place.latitude = fix.latitude;
            place.longitude = fix.longitude;
            place.source = "browser";
            return Ok(place);
        }
        if let Some(place) = nearest(fix.latitude, fix.longitude, 150.0) {
            return Ok(Detected {
                city: place.name.to_string(),
                region: String::new(),
                country: place.country.to_string(),
                latitude: fix.latitude,
                longitude: fix.longitude,
                source: "browser",
            });
        }
        anyhow::bail!("browser location could not be resolved");
    }
    ip_api(&client).await
}

async fn reverse_nominatim(client: &reqwest::Client, fix: GeoFix) -> anyhow::Result<Detected> {
    let lat = fix.latitude.to_string();
    let lon = fix.longitude.to_string();
    let value: serde_json::Value = client
        .get("https://nominatim.openstreetmap.org/reverse")
        .query(&[
            ("format", "jsonv2"),
            ("lat", lat.as_str()),
            ("lon", lon.as_str()),
            ("zoom", "10"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let address = &value["address"];
    let city = [
        "city",
        "town",
        "municipality",
        "village",
        "state_district",
        "county",
    ]
    .iter()
    .find_map(|key| address[key].as_str())
    .unwrap_or("")
    .trim()
    .to_string();
    if city.is_empty() {
        anyhow::bail!("reverse geocode returned no city");
    }
    Ok(Detected {
        city,
        region: address["state"].as_str().unwrap_or("").trim().to_string(),
        country: address["country"].as_str().unwrap_or("").trim().to_string(),
        latitude: fix.latitude,
        longitude: fix.longitude,
        source: "browser",
    })
}

#[derive(Deserialize)]
struct IpApi {
    status: String,
    country: String,
    #[serde(rename = "regionName")]
    region: String,
    city: String,
    lat: f64,
    lon: f64,
}

async fn ip_api(client: &reqwest::Client) -> anyhow::Result<Detected> {
    // fields omits `query`, which is the public IP. The IP is not stored.
    let body: IpApi = client
        .get("http://ip-api.com/json?fields=status,country,regionName,city,lat,lon")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if body.status != "success" || body.city.trim().is_empty() {
        anyhow::bail!("ip location lookup failed");
    }
    Ok(Detected {
        city: body.city.trim().to_string(),
        region: body.region.trim().to_string(),
        country: body.country.trim().to_string(),
        latitude: body.lat,
        longitude: body.lon,
        source: "ip",
    })
}

fn find_place(name: &str) -> Option<&'static Place> {
    let name = name.trim().to_ascii_lowercase();
    PLACES.iter().find(|place| {
        place.name.eq_ignore_ascii_case(&name) || place.aliases.iter().any(|alias| *alias == name)
    })
}

fn nearby_names(lat: f64, lon: f64, country: &str, anchor: &str, limit: usize) -> Vec<String> {
    let mut ranked: Vec<(&Place, f64)> = PLACES
        .iter()
        .filter(|place| place.country.eq_ignore_ascii_case(country.trim()))
        .filter(|place| !place.name.eq_ignore_ascii_case(anchor.trim()))
        .map(|place| (place, distance_km(lat, lon, place.lat, place.lon)))
        .filter(|(_, km)| *km <= 600.0)
        .collect();
    ranked.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked
        .into_iter()
        .take(limit)
        .map(|(place, _)| place.name.to_string())
        .collect()
}

fn nearest(lat: f64, lon: f64, max_km: f64) -> Option<&'static Place> {
    PLACES
        .iter()
        .map(|place| (place, distance_km(lat, lon, place.lat, place.lon)))
        .filter(|(_, km)| *km <= max_km)
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(place, _)| place)
}

fn distance_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let radius = 6371.0;
    let d_lat = (lat2 - lat1).to_radians();
    let d_lon = (lon2 - lon1).to_radians();
    let a = (d_lat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (d_lon / 2.0).sin().powi(2);
    2.0 * radius * a.sqrt().asin()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chennai_starts_with_the_nearest_cities() {
        let nearby = nearby_names(13.0827, 80.2707, "India", "Chennai", 2);
        assert_eq!(
            nearby,
            vec!["Bengaluru".to_string(), "Coimbatore".to_string()]
        );
    }

    #[test]
    fn bangalore_alias_retargets_the_saved_city() {
        let config = retarget(SearchConfig {
            location: "bangalore".into(),
            follow_location: false,
            ..SearchConfig::default()
        });
        assert_eq!(config.location, "bangalore");
        assert_eq!(config.target_country, "India");
        assert_eq!(config.nearby[0], "Coimbatore");
    }
}
