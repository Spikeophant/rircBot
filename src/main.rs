use irc::client::prelude::*;
use clap::Parser;
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::error::Error;
use std::time::Duration;
use tokio::time::sleep;
use futures_util::StreamExt;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use webpki_roots;
use std::sync::Arc;


#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// IRC server address
    #[arg(short, long)]
    server: String,

    /// IRC server port
    #[arg(short, long, default_value_t = 6697)]
    port: u16,

    /// IRC channel to join
    #[arg(short, long)]
    channel: String,

    /// Bot's nickname
    #[arg(short, long, default_value = "RustWeatherBot")]
    nickname: String,

    /// Use TLS
    #[arg(short, long, default_value_t = true)]
    use_tls: bool,
}

struct WeatherBot {
    config: Config,
    nick_locations: HashMap<String, String>,
}

impl WeatherBot {
    fn new(args: Args) -> Result<Self, Box<dyn Error>> {
        let mut config = Config {
            nickname: Some(args.nickname),
            server: Some(args.server),
            port: Some(args.port),
            channels: vec![args.channel],
            use_tls: Some(args.use_tls),
            ..Config::default()
        };

        if args.use_tls {
            let mut root_store = RootCertStore::empty();
            root_store.add(
                webpki_roots::TLS_SERVER_ROOTS
                    .0
                    .iter()
                    .map(|ta| {
                        rustls::OwnedTrustAnchor::from_subject_spki_name_constraints(
                            ta.subject,
                            ta.spki,
                            ta.name_constraints,
                        )
                    })
            ).unwrap();

            let tls_config = ClientConfig::builder()
                .with_safe_defaults()
                .with_root_certificates(root_store)
                .with_no_client_auth();

            config.tls_config = Some(Arc::new(tls_config));
        }

        Ok(WeatherBot {
            config,
            nick_locations: HashMap::new(),
        })
    }
    async fn run(&mut self) -> Result<(), Box<dyn Error>> {
        loop {
            match self.connect_and_run().await {
                Ok(_) => println!("Bot disconnected. Attempting to reconnect..."),
                Err(e) => println!("Error: {}. Attempting to reconnect...", e),
            }
            sleep(Duration::from_secs(5)).await;
        }
    }

    async fn connect_and_run(&mut self) -> Result<(), Box<dyn Error>> {
        let mut client = Client::from_config(self.config.clone()).await?;
        client.identify()?;

        let mut stream = client.stream()?;

        while let Some(message) = stream.next().await {
            match message {
                Ok(message) => self.handle_message(&client, message).await?,
                Err(e) => eprintln!("Error receiving message: {}", e),
            }
        }

        Ok(())
    }

    async fn handle_message(&mut self, client: &Client, message: Message) -> Result<(), Box<dyn Error>> {
        if let Command::PRIVMSG(channel, content) = message.command {
            let nick = message.prefix.and_then(|p| match p {
                Prefix::Nickname(nick, _, _) => Some(nick),
                _ => None,
            });

            if let Some(nick) = nick {
                if let Some(query) = self.parse_weather_query(&content, &nick) {
                    self.send_weather_data(client, &channel, &nick, &query).await?;
                }
            }
        }
        Ok(())
    }

    fn parse_weather_query(&mut self, content: &str, nick: &str) -> Option<String> {
        let re_location = Regex::new(r"!w ([a-zA-Z,\s]+)").unwrap();
        let re_zip = Regex::new(r"!w (\d+)").unwrap();
        let re_nick = Regex::new(r"!w ([^\d\s]+)").unwrap();

        if content == "!w" {
            self.nick_locations.get(nick).cloned()
        } else if let Some(caps) = re_location.captures(content) {
            let query = caps[1].replace(" ", "+").replace(",", "+");
            self.nick_locations.insert(nick.to_string(), query.clone());
            Some(query)
        } else if let Some(caps) = re_zip.captures(content) {
            let query = format!("{},+USA", &caps[1]);
            self.nick_locations.insert(nick.to_string(), query.clone());
            Some(query)
        } else if let Some(caps) = re_nick.captures(content) {
            let target_nick = &caps[1];
            self.nick_locations.get(target_nick).cloned()
        } else {
            None
        }
    }

    async fn send_weather_data(&self, client: &Client, channel: &str, nick: &str, query: &str) -> Result<(), Box<dyn Error>> {
        match self.get_weather(query).await {
            Ok(data) => {
                let response = self.format_response(&data, query);
                let full_response = format!("{}'s weather: {}", nick, response);
                for chunk in full_response.chars().collect::<Vec<char>>().chunks(400) {
                    client.send_privmsg(channel, chunk.iter().collect::<String>())?;
                }
            }
            Err(e) => {
                client.send_privmsg(channel, format!("Error: Could not get weather data for {}. {}", query, e))?;
            }
        }
        Ok(())
    }

    async fn get_weather(&self, query: &str) -> Result<Value, Box<dyn Error>> {
        let url = format!("https://wttr.in/{}?format=j1", query);
        let response = reqwest::get(&url).await?.json::<Value>().await?;
        Ok(response)
    }

    fn format_response(&self, response: &Value, query: &str) -> String {
        let location = response["nearest_area"][0]["areaName"][0]["value"].as_str().unwrap_or(query);
        let current = &response["current_condition"][0];
        let current_temp = current["temp_F"].as_str().unwrap_or("N/A").parse::<i32>().unwrap_or(0);
        let current_temp_c = current["temp_C"].as_str().unwrap_or("N/A").parse::<i32>().unwrap_or(0);
        let current_humidity = current["humidity"].as_str().unwrap_or("N/A");
        let current_temp_emoji = self.get_emoji(current_temp);

        let today_weather = &response["weather"][0];
        let high_temp = today_weather["maxtempF"].as_str().unwrap_or("N/A").parse::<i32>().unwrap_or(0);
        let high_temp_emoji = self.get_emoji(high_temp);
        let low_temp = today_weather["mintempF"].as_str().unwrap_or("N/A").parse::<i32>().unwrap_or(0);
        let low_temp_emoji = self.get_emoji(low_temp);

        let current_conditions = current["weatherDesc"][0]["value"].as_str().unwrap_or("Unknown");
        let current_emoji = self.get_condition_emoji(current["weatherCode"].as_str().unwrap_or("").parse::<i32>().unwrap_or(0));
        let current_color = self.get_temp_color(current_temp);
        let high_temp_color = self.get_temp_color(high_temp);
        let low_temp_color = self.get_temp_color(low_temp);

        let current_str = format!(
            "Conditions: {} \x03{}{}. Humidity: {}%. \
         Temp: {}\x03{}{}\u{00B0}F {}C\x0F. \
         High: {}\x03{}{}\u{00B0}F\x0F. Low: {}\x03{}{}\u{00B0}F\x0F",
            current_emoji, current_color, current_conditions, current_humidity,
            current_temp_emoji, current_color, current_temp, current_temp_c,
            high_temp_emoji, high_temp_color, high_temp,
            low_temp_emoji, low_temp_color, low_temp
        );

        let tomorrow_weather = &response["weather"][1];
        let tomorrow_high_temp = tomorrow_weather["maxtempF"].as_str().unwrap_or("N/A").parse::<i32>().unwrap_or(0);
        let tomorrow_high_temp_emoji = self.get_emoji(tomorrow_high_temp);
        let tomorrow_low_temp = tomorrow_weather["mintempF"].as_str().unwrap_or("N/A").parse::<i32>().unwrap_or(0);
        let tomorrow_low_temp_emoji = self.get_emoji(tomorrow_low_temp);

        let tomorrow_conditions = tomorrow_weather["hourly"][4]["weatherDesc"][0]["value"].as_str().unwrap_or("Unknown");
        let tomorrow_temp = tomorrow_weather["hourly"][4]["tempF"].as_str().unwrap_or("N/A").parse::<i32>().unwrap_or(0);
        let tomorrow_temp_c = tomorrow_weather["hourly"][4]["tempC"].as_str().unwrap_or("N/A").parse::<i32>().unwrap_or(0);
        let tomorrow_humidity = tomorrow_weather["hourly"][4]["humidity"].as_str().unwrap_or("N/A");
        let tomorrow_temp_emoji = self.get_emoji(tomorrow_temp);
        let tomorrow_color = self.get_temp_color(tomorrow_temp);
        let tomorrow_high_temp_color = self.get_temp_color(tomorrow_high_temp);
        let tomorrow_low_temp_color = self.get_temp_color(tomorrow_low_temp);
        let tomorrow_emoji = self.get_condition_emoji(tomorrow_weather["hourly"][4]["weatherCode"].as_str().unwrap_or("").parse::<i32>().unwrap_or(0));

        let tomorrow_str = format!(
            "Conditions: {}{}. Humidity: {}%. \
         Noon: {}\x03{}{}\u{00B0}F {}C\x0F. \
         High: {}\x03{}{}\u{00B0}F\x0F. Low: {}\x03{}{}\u{00B0}F\x0F",
            tomorrow_emoji, tomorrow_conditions, tomorrow_humidity,
            tomorrow_temp_emoji, tomorrow_color, tomorrow_temp, tomorrow_temp_c,
            tomorrow_high_temp_emoji, tomorrow_high_temp_color, tomorrow_high_temp,
            tomorrow_low_temp_emoji, tomorrow_low_temp_color, tomorrow_low_temp
        );

        let day_after_weather = &response["weather"][2];
        let day_after_high_temp = day_after_weather["maxtempF"].as_str().unwrap_or("N/A").parse::<i32>().unwrap_or(0);
        let day_after_high_temp_emoji = self.get_emoji(day_after_high_temp);
        let day_after_low_temp = day_after_weather["mintempF"].as_str().unwrap_or("N/A").parse::<i32>().unwrap_or(0);
        let day_after_low_temp_emoji = self.get_emoji(day_after_low_temp);

        let day_after_conditions = day_after_weather["hourly"][4]["weatherDesc"][0]["value"].as_str().unwrap_or("Unknown");
        let day_after_temp = day_after_weather["hourly"][4]["tempF"].as_str().unwrap_or("N/A").parse::<i32>().unwrap_or(0);
        let day_after_temp_c = day_after_weather["hourly"][4]["tempC"].as_str().unwrap_or("N/A").parse::<i32>().unwrap_or(0);
        let day_after_humidity = day_after_weather["hourly"][4]["humidity"].as_str().unwrap_or("N/A");
        let day_after_temp_emoji = self.get_emoji(day_after_temp);
        let day_after_color = self.get_temp_color(day_after_temp);
        let day_after_high_color = self.get_temp_color(day_after_high_temp);
        let day_after_low_color = self.get_temp_color(day_after_low_temp);
        let day_after_emoji = self.get_condition_emoji(day_after_weather["hourly"][4]["weatherCode"].as_str().unwrap_or("").parse::<i32>().unwrap_or(0));

        let day_after_str = format!(
            "Conditions: {}{}. Humidity: {}%. \
         Noon: {}\x03{}{}\u{00B0}F {}C\x0F. \
         High: {}\x03{}{}\u{00B0}F\x0F. Low: {}\x03{}{}\u{00B0}F\x0F",
            day_after_emoji, day_after_conditions, day_after_humidity,
            day_after_temp_emoji, day_after_color, day_after_temp, day_after_temp_c,
            day_after_high_temp_emoji, day_after_high_color, day_after_high_temp,
            day_after_low_temp_emoji, day_after_low_color, day_after_low_temp
        );

        format!("{}: {} | Tomorrow: {} | Day After: {}", location, current_str, tomorrow_str, day_after_str)
    }
    fn get_emoji(&self, temp: i32) -> &str {
        if temp > 85 {
            "🥵 "
        } else if temp >= 70 {
            "😎️ "
        } else if temp < 32{
            "🥶️ "
        } else {
            "🧥️ "
        }
    }


    fn get_condition_emoji(&self, condition_code: i32) -> &'static str {
        match condition_code {
            113 => "☀️",  // Sunny
            116 => "⛅️",  // Partly Cloudy
            119 | 122 => "☁️",  // Very Cloudy
            143 | 248 | 260 => "🌫️",  // Foggy
            176 | 179 | 182 | 185 | 263 | 266 | 281 | 284 | 293 | 296 | 299 | 302 | 305 | 308 | 311 | 314 | 317 |
            350 | 353 | 359 | 362 | 365 | 374 | 377 => "🌧️",  // LightShowers to Light Sleet
            200 | 386 | 389 => "🌩️🌧️",  // Thundery Showers
            392 => "🌩️🌨️",  // Thundery Snow
            227 | 320 | 323 | 326 | 368 => "🌨️",  // Snow
            230 | 329 | 332 | 335 | 338 | 371 | 395 => "🌨️❄️",  // Heavy Snow
            _ => "✨",  // Unknown/Unsupported Code
        }
    }

    fn get_temp_color(&self, temp: i32) -> &'static str {
        if temp > 85 {
            "04"  // Red
        } else if temp > 70 {
            "07"  // Orange
        } else if temp < 32 {
            "12"  // Light Blue
        } else {
            "03"  // Green
        }
    }

}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();

    let mut bot = WeatherBot::new(args);
    bot.run().await
}