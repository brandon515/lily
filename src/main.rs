pub mod kobold;
pub mod discord;

use songbird::{driver::DecodeMode, Songbird};
use poise::serenity_prelude as serenity;
use serenity::GatewayIntents;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>>{
    dotenv::dotenv().ok();
    tokio::spawn(async move {
        let token = std::env::var("DISCORD_TOKEN").expect("Expected a token in the environment");

        let intents = GatewayIntents::GUILD_MESSAGES
            | GatewayIntents::DIRECT_MESSAGES
            | GatewayIntents::MESSAGE_CONTENT
            | GatewayIntents::GUILDS
            | GatewayIntents::GUILD_VOICE_STATES
            | GatewayIntents::GUILD_MEMBERS
            | GatewayIntents::GUILD_PRESENCES;

        let songbird_config = songbird::Config::default().decode_mode(DecodeMode::Decode);

        let manager = Songbird::serenity_from_config(songbird_config);
        let clone_manager = Arc::clone(&manager);
        
        let framework = discord::get_framework(clone_manager);

        let mut client = serenity::Client::builder(&token, intents)
            .framework(framework)
            .voice_manager_arc(manager)
            .await
            .expect("Error creating client");

        if let Err(why) = client.start().await {
            println!("Client error: {why:?}");
        }
    });

    tokio::signal::ctrl_c().await.unwrap();
    println!("Program exited gracefully");
    Ok(())
}
