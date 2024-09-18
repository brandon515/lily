use dashmap::DashMap;
use poise::serenity_prelude as serenity;
use ::serenity::{
  all::{
    ChannelId as PoiseChannelId, Guild, Http, Mentionable, PartialGuild
  }, 
  async_trait
};
use songbird::{
  model::payload::Speaking,
  CoreEvent, EventContext, 
  EventHandler as VoiceEventHandler, 
  Songbird
};
use tokio::sync::{mpsc::UnboundedSender, Mutex as TokioMutex};
use futures_util::SinkExt;
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;
use std::{
  fmt::Debug, sync::{
    atomic::{
      AtomicBool, 
      Ordering,
    }, 
    Arc,
  }
};

use crate::{
  storage::{
    create_storage_thread, StorageMessage
  }, whisper::{spawn_whisper_thread, WhisperSink}
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

#[derive(Debug)]
struct Speaker {
  id: String,
  message_send: Option<TokioMutex<WhisperSink>>,
}

struct InnerReceiver{
  last_tick_was_empty: AtomicBool,
  known_ssrcs: dashmap::DashMap<u32, Speaker>,
  storage_tx: UnboundedSender<StorageMessage>,
}

pub fn get_http() -> Http{
  let token = std::env::var("DISCORD_TOKEN").expect("Expected DISCORD_URL in the environment variables");
  Http::new(&token)
}

pub async fn start_broadcasting(channel_id: u64){
  let http = get_http();
  if let Ok(channel_op) = http.get_channel(channel_id.into()).await{
    if let Some(channel) = channel_op.guild(){
      if let Err(err) = channel.broadcast_typing(&http).await{
        println!("Unable to broadcast typing: {}", err);
      }
    }
  }
}

fn split_string(codepoints_over: usize, original_string: String) -> (String, String){
  (original_string.char_indices().rev().nth(codepoints_over).map(|(i, _)| &original_string[..i]).unwrap().to_string(),
  original_string.char_indices().rev().nth(codepoints_over).map(|(i, _)| &original_string[i..]).unwrap().to_string())
}

pub async fn send_discord_message(message: String, channel_id: u64){
  let http = get_http();
  if let Ok(channel_op) = http.get_channel(channel_id.into()).await{
    if let Some(channel) = channel_op.guild(){
      if let Err(err) = channel.say(&http, message.clone()).await{
        if let serenity::Error::Model(serenity::ModelError::MessageTooLong(num_codepoints)) = err{
          let (first, second) = split_string(num_codepoints, message);
          check_msg(channel.say(&http, first).await);
          check_msg(channel.say(&http, second).await);
        }else{
          println!("Unable to send message to discord: {}", err);
        }
      }
    }
  }
}

impl Reciever{
  pub fn new(storage_tx: UnboundedSender<StorageMessage>, channel: PoiseChannelId, guild: PartialGuild) -> Self{
    Self { inner: Arc::new(InnerReceiver{
        last_tick_was_empty: AtomicBool::new(true),
        known_ssrcs: DashMap::new(),
        storage_tx,
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
        ssrc,
        user_id,
        ..
      }) => {
        if let Some(user) = user_id{
          let member = match self.guild.member(get_http(), serenity::all::UserId::new(user.0)).await{
            Ok(r) => r,
            Err(err) => {
              println!("Not able to get user name with user id: {}", err);
              return None;
            }
          };
          if let Some(mut existing_speaker) = self.inner.known_ssrcs.get_mut(&ssrc){
            if member.user.bot{
              existing_speaker.id = "Bot".to_string();
            }else{
              existing_speaker.id = member.display_name().to_string();
            }
          }else{
            if member.user.bot{
              self.inner.known_ssrcs.insert(*ssrc, Speaker { 
                id: "Bot".to_string(), 
                message_send: None,
              });
            }else{
              self.inner.known_ssrcs.insert(*ssrc, Speaker { 
                id: member.user.global_name.unwrap(), 
                message_send: None,
              });
            }
          }
        }
      },
      EventContext::VoiceTick(tick) => {
        let speaking = tick.speaking.len();
        let _total_participants = speaking + tick.silent.len();
        let _last_tick_was_empty = self.inner.last_tick_was_empty.load(Ordering::SeqCst);

        for (ssrc, data) in &tick.speaking {
          let mut speaker = match self.inner.known_ssrcs.get_mut(ssrc){
            Some(s) => s,
            None => {
              println!("No SSRC for {}", ssrc);
              continue;
            }
          };
          if speaker.id == "Bot"{
            continue;
          }
          if let Some(decoded_voice) = &data.decoded_voice {
            match &speaker.message_send{
              Some(r) => {
                let mut error = false;
                if let Err(err) = r.lock().await.feed(
                  TungsteniteMessage::Binary(resample_discord_to_bytes(decoded_voice.to_vec()))
                ).await{
                  println!("Error sending data to whisper thread: {}", err);
                  error = true;
                };
                if let Err(err) = r.lock().await.flush().await{
                  println!("Error sending data to whisper thread: {}", err);
                  error = true;
                }
                if error{
                  speaker.message_send = None;
                  continue;
                }
              },
              None => {
                println!("Created new speaker thread");
                let new_mutex = TokioMutex::new(
                  spawn_whisper_thread(self.inner.storage_tx.clone(), speaker.id.clone(), self.default_channel.get()).await
                );
                if let Err(err) = new_mutex.lock().await.feed(
                  TungsteniteMessage::Binary(resample_discord_to_bytes(decoded_voice.to_vec()))
                ).await{
                  println!("Error creating new whisper thread: {}", err);
                  speaker.message_send = None;
                  continue;
                };
                if let Err(err) = new_mutex.lock().await.flush().await{
                  println!("Error creating new whisper thread: {}", err);
                  speaker.message_send = None;
                  continue;
                }
                speaker.message_send = Some(new_mutex);
              },
            };
          }else{
            println!("Decode disabled");
          }
        }

        for ssrc in &tick.silent{
          if let Some(mut speaker) = self.inner.known_ssrcs.get_mut(ssrc){
            if let Some(message_send) = &speaker.message_send{
              if let Err(err) = message_send.lock().await.close().await{
                println!("Couldn't close whisper connection: {}", err);
              }
              speaker.message_send = None;
            }
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
  storage_tx: UnboundedSender<StorageMessage>,
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

    let partial_guild = Guild::get(get_http(), guild_id).await?;

    let evt_receiver = Reciever::new(ctx.data().storage_tx.clone(), ctx.channel_id(), partial_guild);

    handler.add_global_event(CoreEvent::DriverConnect.into(), evt_receiver.clone());
    handler.add_global_event(CoreEvent::SpeakingStateUpdate.into(), evt_receiver.clone());
    handler.add_global_event(CoreEvent::VoiceTick.into(), evt_receiver.clone());

    check_msg(ctx.reply(format!("Joined {}", connect_to.mention())).await);
  }

  Ok(())
}

async fn poise_event_handler(
  _ctx: &serenity::Context,
  event: &serenity::FullEvent,
  _framework: poise::FrameworkContext<'_, Data, Error>,
  data: &Data,
) -> Result<(), Error>{
  match event {
    serenity::FullEvent::Message{new_message} =>{
      if new_message.author.bot{
        return Ok(());
      }
      data.storage_tx.send(StorageMessage{
        channel: new_message.channel_id.get(),
        message: new_message.content.clone(),
        author: new_message.author.global_name.as_ref().unwrap().clone(),
      })?;
    },
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
              storage_tx: create_storage_thread(),
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