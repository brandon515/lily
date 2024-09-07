use dashmap::DashMap;
use futures_util::{
  SinkExt, 
  StreamExt
};
use poise::serenity_prelude as serenity;
use serde::Deserialize;
use ::serenity::{
  all::{
    ChannelId as PoiseChannelId, CreateMessage, EditMessage, Http, Mentionable, Message
  }, 
  async_trait
};
use songbird::{
  model::{
    id::UserId as VoiceId, 
    payload::Speaking,
  }, 
  CoreEvent, EventContext, 
  EventHandler as VoiceEventHandler, 
  Songbird
};
use tokio::sync::mpsc::UnboundedSender;
use tokio_tungstenite::{
  connect_async,
  tungstenite::Message as TungsteniteMessage,
};
use std::{
  collections::HashMap, fmt::Debug, fs::File, io::Read, sync::{
      atomic::{
      AtomicBool, 
      Ordering,
    }, 
    Arc,
  }
};

type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;
type CommandResult = Result<(), Error>;
/*type DiscordStream = futures_util::stream::SplitStream<
  tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<
      tokio::net::TcpStream>>>;*/
type DiscordSink = futures_util::stream::SplitSink<
  tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<
      tokio::net::TcpStream>>, TungsteniteMessage>;

struct TranscriptionMessage{
  id: VoiceId,
  text: String,
}

#[derive(Deserialize)]
struct WhisperResponse{
  text: String,
}

#[derive(Clone)]
pub struct Reciever{
  inner: Arc<InnerReceiver>,
}

#[derive(Clone, Debug)]
struct Speaker {
  id: VoiceId,
  message_send: Option<UnboundedSender<Vec<i16>>>,
}

struct InnerReceiver{
  last_tick_was_empty: AtomicBool,
  known_ssrcs: dashmap::DashMap<u32, Speaker>,
  trans_tx: UnboundedSender<TranscriptionMessage>,
}

impl Reciever{
  pub fn new(channel: PoiseChannelId) -> Self{
    let(trans_tx,mut rx) = tokio::sync::mpsc::unbounded_channel::<TranscriptionMessage>();
    tokio::spawn(async move{
      let token = std::env::var("DISCORD_TOKEN").expect("Expected a token in the environment");
      let http = Http::new(&token);
      let mut message_log = HashMap::<VoiceId, Message>::new();
      while let Some(msg) = rx.recv().await{
        if msg.text == ""{
          message_log.remove(&msg.id);
          continue;
        }
        if let Some(discord_msg) = message_log.get_mut(&msg.id){
          if let Err(err) = discord_msg.edit(&http, EditMessage::new().content(
            format!("<@{}>: {}", msg.id.to_string(), msg.text)
          )).await{
            println!("Unable to edit message with ID {}: {}", discord_msg.id, err);
          }
        }else{
          match channel.send_message(&http, CreateMessage::new().content(
            format!("<@{}>: {}", msg.id.to_string(), msg.text)
          )).await{
            Ok(r) => {
              message_log.insert(msg.id, r);
            },
            Err(err) => {
              println!("Error sending message: {}", err);
            }
          }
        }
      }
    });
    Self { inner: Arc::new(InnerReceiver{
        last_tick_was_empty: AtomicBool::new(true),
        known_ssrcs: DashMap::new(),
        trans_tx,
      }),
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
          if let Some(mut existing_speaker) = self.inner.known_ssrcs.get_mut(&ssrc){
            existing_speaker.id = *user;
          }else{
            self.inner.known_ssrcs.insert(*ssrc, Speaker { 
              id: *user, 
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
                id: VoiceId(0),
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
                let(tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                let discord_tx = self.inner.trans_tx.clone();
                let id = speaker.id.clone();
                tokio::spawn(async move{
                  let purge_msg = TranscriptionMessage{
                    id,
                    text: "".to_string(),
                  };
                  let new_rx = discord_tx.clone();
                  let mut write = spawn_discord_thread(new_rx, id).await;
                  while let Some(data) = rx.recv().await{
                    let bytes = resample_discord_to_bytes(data);
                    if let Err(_) = write.feed(TungsteniteMessage::binary(bytes)).await{
                      discord_tx.send(purge_msg).unwrap();
                      return;
                    }
                    if let Err(_) = write.flush().await{
                      discord_tx.send(purge_msg).unwrap();
                      return;
                    }
                  }
                });
                speaker.message_send = Some(tx);
                if let Some(r) = &speaker.message_send{
                  r
                }else{
                  println!("Error creating new tx for speaker with SSRC {ssrc:?}");
                  continue;
                }
              },
            };
            if let Err(err) = tx.send(decoded_voice.clone()){
              println!("Err sending voice data over channel: {}", err);
              speaker.message_send = None;
            };
          }else{
            println!("Decode disabled");
          }
        }
        for ssrc in &tick.silent{
          if let Some(mut speaker) = self.inner.known_ssrcs.get_mut(&ssrc){
            if let Some(_) = &speaker.message_send{
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

  if let Ok(handler_lock) = manager.join(guild_id, connect_to).await{
    let mut handler = handler_lock.lock().await;

    let evt_receiver = Reciever::new(ctx.channel_id());

    handler.add_global_event(CoreEvent::DriverConnect.into(), evt_receiver.clone());
    handler.add_global_event(CoreEvent::SpeakingStateUpdate.into(), evt_receiver.clone());
    handler.add_global_event(CoreEvent::VoiceTick.into(), evt_receiver.clone());
    //handler.add_global_event(CoreEvent::RtpPacket.into(), evt_receiver.clone());

    check_msg(ctx.reply(format!("Joined {}", connect_to.mention())).await);
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

fn _get_wav_data(speaker: Vec<i16>) -> Result<Vec<u8>, Error>{
  let spec = hound::WavSpec {
    channels: 2,
    sample_rate: 48000,
    bits_per_sample: 16,
    sample_format: hound::SampleFormat::Int,
  };
  let mut hound_writer = hound::WavWriter::create("dummy.wav", spec).unwrap();
  //let writer_len = speaker.buffer.len().try_into().unwrap();
  //let mut writer = hound_writer.get_i16_writer(writer_len);
  for sample in speaker.iter(){
    hound_writer.write_sample(*sample)?
  }
  hound_writer.flush()?;
  let mut f = File::options().read(true).open("dummy.wav").unwrap();
  let mut buf = Vec::new();
  let bytes_read = f.read_to_end(&mut buf)?;
  println!("Bytes from dummy.wav: {}", bytes_read);
  Ok(buf)
}

fn resample_discord_to_bytes(sound_data: Vec<i16>) -> Vec<u8>{
  let resampled: Vec<i16> = sound_data.chunks_exact(6).map(|x|{
    ((x[0] as i64 + x[2] as i64 + x[4] as i64)/3) as i16
  }).collect();
  let byte_chunks: Vec<[u8; 2]> = resampled.iter().map(|x| {x.to_le_bytes()}).collect();
  let sound_bytes = byte_chunks.into_flattened();
  sound_bytes
}

async fn spawn_discord_thread(discord_rx: UnboundedSender<TranscriptionMessage>, id: VoiceId) -> DiscordSink{
  let(ws_stream, _) = connect_async(
    "ws://localhost:8000/v1/audio/transcriptions?language=en"
  ).await.expect("Failed to connect to whisper server");
  let (write, read) = ws_stream.split();
  let new_rx = discord_rx.clone();
  tokio::spawn(async move{
    let local_tx = &new_rx.clone();
    read.for_each(|mes| async{
      if let Ok(r) =  serde_json::from_str::<WhisperResponse>(&mes.unwrap().into_text().unwrap()){
          if let Err(err) = local_tx.send(TranscriptionMessage{
            id,
            text: r.text,
          }){
            println!("Error sending trascription message: {}", err);
          };
      }
    }).await;
  });
  write
}