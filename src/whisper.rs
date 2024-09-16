use tokio_tungstenite::connect_async;
use tokio::sync::mpsc::UnboundedSender;
use futures_util::StreamExt;
use serde::Deserialize;
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;
use crate::storage::StorageMessage;

#[derive(Deserialize)]
struct WhisperResponse{
  text: String,
}

type WhisperSink = futures_util::stream::SplitSink<
  tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<
      tokio::net::TcpStream>>, TungsteniteMessage>;

pub async fn spawn_whisper_thread(storage_tx: UnboundedSender<StorageMessage>, display_name: String, channel: u64) -> WhisperSink{
  let(ws_stream, _) = connect_async(
    std::env::var("WHISPER_URL").expect("Expected WHISPER_URL in the environment variables")
  ).await.expect("Failed to connect to whisper server");
  let (write, mut read) = ws_stream.split();
  tokio::spawn(async move{
    let mut full_transcription = String::new();
    while let Some(mes_res) = read.next().await{
      let mes = match mes_res{
        Ok(r) => r,
        Err(err) => {
          println!("Error getting data from whisper server: {}", err);
          break;
        }
      };
      let json_text = match mes.into_text(){
        Ok(r) => r,
        Err(err) => {
          println!("Data received from whisper server could not be made into utf-8 string: {}", err);
          break;
        }
      };
      let json_mes: WhisperResponse = match serde_json::from_str(&json_text){
        Ok(r) => r,
        Err(_) => {
          //println!("Not able to put string from whisper server into a json object: {}", err);
          break;
        }
      };
      if json_mes.text != String::new(){
        full_transcription = json_mes.text;
      }
    }
    if full_transcription == String::new(){
      return;
    }
    if let Err(err) = storage_tx.send(StorageMessage{
      channel,
      author: display_name,
      message: full_transcription,
    }){
      println!("Error sending trascription message: {}", err);
    };
  });
  write
}