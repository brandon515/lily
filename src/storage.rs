use rusqlite::{
  params, 
  Connection,
};
use tokio::sync::mpsc::{
  unbounded_channel as tokio_channel,
  UnboundedSender,
};

use crate::kobold::{
  spawn_kobold_thread, KoboldRequest, StoredMessage
};

#[derive(Debug, Clone)]
pub struct StorageMessage{
  pub message: String,
  pub author: String,
  pub channel: u64,
}

pub fn create_storage_thread() -> UnboundedSender<StorageMessage>{
  let (sqlite_tx, mut sqlite_rx) = tokio_channel::<StorageMessage>();
  let kobold_tx = spawn_kobold_thread(sqlite_tx.clone());
  tokio::spawn(async move{
    let conn = Connection::open("./memory.db").expect("Not able to open SQLITE db called memory.db");
    conn.execute(
      "CREATE TABLE IF NOT EXISTS messages(
        id INTEGER PRIMARY KEY,
        author TEXT NOT NULL,
        message TEXT NOT NULL,
        channel INTEGER NOT NULL
    )", ()).unwrap();
    while let Some(message_to_store) = sqlite_rx.recv().await{
      let possible_activation_message = message_to_store.clone();
      if let Err(err) = conn.execute("INSERT INTO messages (author, message, channel) VALUES (?1, ?2, ?3)", (
        message_to_store.author,
        message_to_store.message,
        message_to_store.channel
      )){
        println!("Can't insert message into SQLITE3 database: {}", err);
      }
      let activation_phrase = std::env::var("ACTIVATION_PHRASE").unwrap().to_lowercase();
      let bot_name = std::env::var("BOT_NAME").unwrap();
      if possible_activation_message.message.to_lowercase().contains(&activation_phrase)
      && possible_activation_message.author != bot_name{
        let mut stmt = conn.prepare("SELECT author, message FROM messages WHERE channel=?1").unwrap();
        let stored_message_iter = match stmt.query_map(
          params![possible_activation_message.channel], 
          |row|{
            Ok(StoredMessage{
              author: row.get(0)?,
              message: row.get(1)?,
            })
          }){
            Ok(r) => r,
            Err(err) => {
              println!("Couldn't retrieve messages from sqlite database: {}", err);
              continue;
            }
          };
        let messages: Vec<StoredMessage> = stored_message_iter.map(|x| {x.unwrap()}).collect();
        if let Err(err) = kobold_tx.send(KoboldRequest{
          origin_channel: possible_activation_message.channel,
          messages,
        }){
          println!("Unable to send context to kobold thread: {}", err);
        }
      }
    }
  });
  sqlite_tx
}