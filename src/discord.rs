use dashmap::DashMap;
use poise::serenity_prelude as serenity;
use ::serenity::{
  all::{
    ChannelId as PoiseChannelId, Guild, GuildId, Http, Mentionable, PartialGuild
  }, 
  async_trait
};
use songbird::{
  model::payload::Speaking,
  CoreEvent, EventContext, 
  EventHandler as VoiceEventHandler, 
  Songbird
};
use tokio::sync::mpsc::UnboundedSender;
use futures_util::SinkExt;
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;
use std::{
  fmt::Debug,
  sync::{
    atomic::{
      AtomicBool, 
      Ordering,
    }, 
    Arc,
  }
};

use crate::{
  whisper::spawn_whisper_thread,
  kobold::{
    self, 
    KoboldMessage
  }
};

type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;
type CommandResult = Result<(), Error>;
/*type DiscordStream = futures_util::stream::SplitStream<
  tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<
      tokio::net::TcpStream>>>;*/




#[derive(Clone)]
pub struct Reciever{
  inner: Arc<InnerReceiver>,
  default_channel: PoiseChannelId,
  guild: PartialGuild,
}

#[derive(Clone, Debug)]
struct Speaker {
  id: String,
  message_send: Option<UnboundedSender<Vec<i16>>>,
}

struct InnerReceiver{
  last_tick_was_empty: AtomicBool,
  known_ssrcs: dashmap::DashMap<u32, Speaker>,
  kobold_tx: UnboundedSender<KoboldMessage>,
}

fn get_http() -> Http{
  let token = std::env::var("DISCORD_TOKEN").expect("Expected DISCORD_URL in the environment variables");
  Http::new(&token)
}

pub async fn send_discord_message(message: String, channel: PoiseChannelId){
  let http = get_http();
  check_msg(channel.say(&http, message).await);
}

impl Reciever{
  pub fn new(kobold_tx: UnboundedSender<KoboldMessage>, channel: PoiseChannelId, guild: PartialGuild) -> Self{
    Self { inner: Arc::new(InnerReceiver{
        last_tick_was_empty: AtomicBool::new(true),
        known_ssrcs: DashMap::new(),
        kobold_tx,
      }),
      default_channel: channel,
      guild,
    }
  }
}

#[async_trait]
impl VoiceEventHandler for Reciever{
  async fn act(&self, ctx: &EventContext<'_>) -> Option<songbird::Event>{
    match ctx{
      EventContext::DriverConnect(_con_data)=> {
        println!("Connected");
      },
      EventContext::SpeakingStateUpdate(Speaking{
        speaking,
        ssrc,
        user_id,
        ..
      }) => {
        println!("Speaking state update: user {:?} has SSRC {:?}, using {:?}", user_id, ssrc, speaking);
        if let Some(user) = user_id{
          let member = match self.guild.member(get_http(), serenity::all::UserId::new(user.0)).await{
            Ok(r) => r,
            Err(err) => {
              println!("Not able to get user name with user id: {}", err);
              return None;
            }
          };
          if let Some(mut existing_speaker) = self.inner.known_ssrcs.get_mut(&ssrc){
            existing_speaker.id = member.display_name().to_string();
          }else{
            self.inner.known_ssrcs.insert(*ssrc, Speaker { 
              id: member.display_name().to_string(), 
              message_send: None,
            });
          }
        }
      },
      EventContext::VoiceTick(tick) => {
        let speaking = tick.speaking.len();
        let _total_participants = speaking + tick.silent.len();
        let _last_tick_was_empty = self.inner.last_tick_was_empty.load(Ordering::SeqCst);

        for (ssrc, data) in &tick.speaking {
          let mut speaker = match self.inner.known_ssrcs.get_mut(&ssrc){
            Some(s) => s,
            None => {
              let new_speaker = Speaker{
                id: String::new(),
                message_send: None,
              };
              self.inner.known_ssrcs.insert(*ssrc, new_speaker);
              if let Some(s) = self.inner.known_ssrcs.get_mut(&ssrc){
                s
              }else{
                println!("Failed to insert new speaker with SSRC {:?}", ssrc);
                continue;
              }
            }
          };
          if let Some(decoded_voice) = &data.decoded_voice {
            let tx = match &speaker.message_send{
              Some(r) => r,
              None => {
                speaker.message_send = Some(spawn_speaker_thread(self.inner.kobold_tx.clone(), speaker.id.clone(), self.default_channel).await);
                if let Some(r) = &speaker.message_send{
                  r
                }else{
                  println!("Error creating new tx for speaker with SSRC {ssrc:?}");
                  continue;
                }
              },
            };
            if let Err(err) = tx.send(decoded_voice.clone()){
              if err.to_string() != "channel closed"{
                println!("Err sending voice data over channel: {}", err);
              }
              let new_tx = spawn_speaker_thread(self.inner.kobold_tx.clone(), speaker.id.clone(), self.default_channel).await;
              if let Err(err) = tx.send(decoded_voice.clone()){
                println!("Error creating new channel for voice data: {}", err);
                speaker.message_send = None;
              }else{
                speaker.message_send = Some(new_tx);
              }
            };
          }else{
            println!("Decode disabled");
          }
        }
      },
      _ => unimplemented!(),
    }
    None
  }
}

pub struct Data {
  songbird: Arc<Songbird>,
  kobold_channels: DashMap<GuildId, UnboundedSender<KoboldMessage>>,
}

#[poise::command(slash_command, prefix_command)]
async fn age(
    ctx: Context<'_>,
    #[description = "Selected user"] user: Option<serenity::User>,
) -> CommandResult {
    let u = user.as_ref().unwrap_or_else(|| ctx.author());
    let response = format!("{}'s account was created at {}", u.name, u.created_at());
    ctx.say(response).await?;
    Ok(())
}

#[poise::command(prefix_command,guild_only)]
async fn mere(ctx: Context<'_>) -> CommandResult{
  let (guild_id, channel_id) = {
    let guild = ctx.guild().unwrap();
    let channel_id = guild
      .voice_states
      .get(&ctx.author().id)
      .and_then(|voice_state| voice_state.channel_id);
    (guild.id, channel_id)
  };

  let connect_to = match channel_id{
    Some(chan) => chan,
    None => {
      check_msg(ctx.reply("Not in a voice channel").await);
      return Ok(());
    }
  };
  

  let manager = ctx.data().songbird.clone();

  if let Ok(handler_lock) = manager.join(guild_id.clone(), connect_to).await{
    let mut handler = handler_lock.lock().await;

    if let Some(kobold_tx) = ctx.data().kobold_channels.get(&guild_id){
      let token = std::env::var("DISCORD_TOKEN").expect("Expected DISCORD_URL in the environment variables");
      let http = Http::new(&token);
      let partial_guild = Guild::get(http, guild_id).await?;

      let evt_receiver = Reciever::new(kobold_tx.value().clone(), ctx.channel_id(), partial_guild);

      handler.add_global_event(CoreEvent::DriverConnect.into(), evt_receiver.clone());
      handler.add_global_event(CoreEvent::SpeakingStateUpdate.into(), evt_receiver.clone());
      handler.add_global_event(CoreEvent::VoiceTick.into(), evt_receiver.clone());

      check_msg(ctx.reply(format!("Joined {}", connect_to.mention())).await);
    }
  }

  Ok(())
}

async fn poise_event_handler(
  _ctx: &serenity::Context,
  event: &serenity::FullEvent,
  framework: poise::FrameworkContext<'_, Data, Error>,
  _data: &Data,
) -> Result<(), Error>{
  match event {
    serenity::FullEvent::Ready { data_about_bot } => {
      data_about_bot.guilds.iter().for_each(|x|{
        if x.unavailable == false{
          let kobold_tx = kobold::spawn_kobold_thread();
          framework.user_data.kobold_channels.insert(x.id, kobold_tx);
        }
      });
    },
    serenity::FullEvent::Message{new_message} =>{
      if new_message.author.bot{
        return Ok(());
      }
      let guild_id = match new_message.guild_id{
        Some(r) => r,
        None => {
          println!("Not able to get Guild ID from messge: {}", new_message.content);
          return Ok(());
        }
      };
      if let Some(kobold_ref) = framework.user_data.kobold_channels.get(&guild_id){
        kobold_ref.value().send(KoboldMessage{
          origin_channel: new_message.channel_id,
          message: new_message.content.clone(),
          author: new_message.author.global_name.as_ref().unwrap().clone(),
        })?;
      }
    },
    serenity::FullEvent::GuildCreate { guild, is_new: _ } => {
      let kobold_tx = kobold::spawn_kobold_thread();
      framework.user_data.kobold_channels.insert(guild.id, kobold_tx);
    }
    _ => {}
  }
  Ok(())
}

fn get_framework_options() -> poise::FrameworkOptions<Data, Error>{
  poise::FrameworkOptions{
    commands: vec![age(), mere()],
    prefix_options: poise::PrefixFrameworkOptions{
      prefix: Some("~".to_string()),
      additional_prefixes: vec![
        poise::Prefix::Literal("Hey Shodan"),
        poise::Prefix::Literal("Hey Shodan,"),
      ],
      ..Default::default()
    },
    command_check: Some(|_ctx| {
      Box::pin(async move {
          Ok(true)
      })
    }),
    event_handler: |ctx, event, framework, data|{
      Box::pin(poise_event_handler(ctx, event, framework, data))
    },
    ..Default::default()
  }
}

pub fn get_framework(songbird: Arc<Songbird>) -> poise::Framework<Data, Error>{
  poise::Framework::builder()
    .options(get_framework_options())
    .setup(|ctx, _ready, framework|{
        Box::pin(async move{
            println!("Logged in as {}", _ready.user.name);
            poise::builtins::register_globally(ctx, &framework.options().commands).await?;
            Ok(Data {
              songbird,
              kobold_channels: DashMap::new(),
            })
        })
    })
    .build()
}

fn check_msg<T>(result: serenity::Result<T>){
  if let Err(why) = result {
    println!("Error sending message {why:?}");
  }
}

fn resample_discord_to_bytes(sound_data: Vec<i16>) -> Vec<u8>{
  let resampled: Vec<i16> = sound_data.chunks_exact(6).map(|x|{
    ((x[0] as i64 + x[2] as i64 + x[4] as i64)/3) as i16
  }).collect();
  let byte_chunks: Vec<[u8; 2]> = resampled.iter().map(|x| {x.to_le_bytes()}).collect();
  let sound_bytes = byte_chunks.into_flattened();
  sound_bytes
}

async fn spawn_speaker_thread(kobold_tx: UnboundedSender<KoboldMessage>, display_name: String, channel: PoiseChannelId) -> UnboundedSender<Vec<i16>>{
  let(tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
  tokio::spawn(async move{
    let local_tx = kobold_tx.clone();
    let mut write = spawn_whisper_thread(local_tx.clone(), display_name.clone(), channel).await;
    while let Some(data) = rx.recv().await{
      let bytes = resample_discord_to_bytes(data);
      if let Err(_) = write.feed(TungsteniteMessage::binary(bytes.clone())).await{
        write = spawn_whisper_thread(local_tx.clone(), display_name.clone(), channel).await;
        write.feed(TungsteniteMessage::Binary(bytes)).await.expect("Unable to recreate whisper thread");
        write.flush().await.expect("Unable to recreate whisper thread");
        continue;
      }
      if let Err(_) = write.flush().await{
        write = spawn_whisper_thread(local_tx.clone(), display_name.clone(), channel).await;
        write.feed(TungsteniteMessage::Binary(bytes)).await.expect("Unable to recreate whisper thread");
        write.flush().await.expect("Unable to recreate whisper thread");
        continue;
      }
    }
  });
  tx
}