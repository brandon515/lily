use rusqlite::{
  params, 
  Connection, 
  Transaction
};
use tokio::sync::mpsc::{
  unbounded_channel as tokio_channel,
  UnboundedSender,
};

use crate::kobold::KoboldMessage;

fn create_sqlite_thread() -> UnboundedSender<KoboldMessage>{
  let (sqlite_tx, sqlite_rx) = tokio_channel::<KoboldMessage>();
  sqlite_tx
}