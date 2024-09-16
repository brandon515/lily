use std::{time::Duration, vec};

use reqwest::{header, StatusCode};
use serde::{
  Serialize,
  Deserialize,
};
use tokio::sync::mpsc::UnboundedSender;
use crate::{
  discord::{
    send_discord_message, 
    start_broadcasting
  },
  storage::StorageMessage,
};

#[derive(Serialize)]
struct KoboldData{
  prompt: String,
  max_new_tokens: u32,
  new_tokens: u32,
  temperature: f32,
  top_p: f32,
  typical_p: f32,
  typical: f32,
  sampler_seed: i64,
  min_p: f32,
  repetition_penalty: f32,
  frequency_penalty: f32,
  presence_penalty: f32,
  top_k: i32,
  skew: f32,
  min_tokens: u32,
  length_penalty: f32,
  early_stopping: bool,
  add_bos_token: bool,
  smoothing_factor: f32,
  smoothing_curve: f32,
  dry_allowed_length: u32,
  dry_multiplier: f32,
  dry_base: f32,
  dry_sequence_breakers: Vec<String>,
  stopping_strings: Vec<String>,
  stop: Vec<String>,
  truncation_length: u32,
  ban_eos_token: bool,
  skip_special_tokens: bool,
  top_a: f32, //maybe this one
  tfs: f32, //maybe this one
  mirostat_mode: u32,
  mirostat_tau: f32,
  mirostat_eta: f32,
  custom_token_bans: String,
  banned_strings: Vec<String>,
  api_type: String,
  api_server: String,
  legecy_api: bool,
  sampler_order: Vec<u32>,
  grammer: String,
  rep_pen: f32,
  rep_pen_range: u32,
  rep_pen_slope: f32,
  repetition_penalty_range: u32,
  seed: i32,
  guidance_scale: f32,
  negative_prompt: String,
  grammer_string: String,
  repeat_penalty: f32,
  tfs_z: f32,
  repeat_last_n: f32,
  n_predict: u32,
  num_predict: u32,
  num_ctx: u32,
  mirostat: f32, //Maybe this one
  ignore_eos: bool,
  stream: bool,
}

impl KoboldData{
  fn new(prompt: String) -> Self{
    let dry_sequence_breakers = vec!["\n".to_string(), ":".to_string(), "\"".to_string(), "*".to_string()]; 
    let breakers = vec![
      "<|eot_id|><|end_of_text|>".to_string(),
      "<|start_header_id|>user<|end_header_id|>".to_string(),
      "<|start_header_id|>assistant<|end_header_id|>".to_string(),
      "<|begin_of_text|><|start_header_id|>system<|end_header_id|>".to_string()];
    Self{
      prompt,
      max_new_tokens: 512,
      new_tokens: 512,
      temperature: 1.0,
      top_p: 1.0,
      typical_p: 1.0,
      typical: 1.0,
      sampler_seed: -1,
      min_p: 0.0,
      repetition_penalty: 1.0,
      frequency_penalty: 0.0,
      presence_penalty: 0.0,
      top_k: 0,
      skew: 0.0,
      min_tokens: 0,
      length_penalty: 0.0,
      early_stopping: false,
      add_bos_token: true,
      smoothing_factor: 0.0,
      smoothing_curve: 1.0,
      dry_allowed_length: 2,
      dry_multiplier: 0.0,
      dry_base: 1.75,
      dry_sequence_breakers,
      stopping_strings: breakers.clone(),
      stop: breakers.clone(),
      truncation_length: 16384,
      ban_eos_token: false,
      skip_special_tokens: true,
      top_a: 0.0, //maybe this one
      tfs: 1.0, //maybe this one
      mirostat_mode: 0,
      mirostat_tau: 5.0,
      mirostat_eta: 0.1,
      custom_token_bans: String::new(),
      banned_strings: Vec::new(),
      api_type: "koboldcpp".to_string(),
      api_server: "http://127.0.0.1:5001/v1".to_string(),
      legecy_api: false,
      sampler_order: vec![6,0,1,3,4,2,5],
      grammer: String::new(),
      rep_pen: 1.0,
      rep_pen_range: 0,
      rep_pen_slope: 1.0,
      repetition_penalty_range: 0,
      seed: -1,
      guidance_scale: 1.0,
      negative_prompt: String::new(),
      grammer_string: String::new(),
      repeat_penalty: 1.0,
      tfs_z: 1.0,
      repeat_last_n: 0.0,
      n_predict: 512,
      num_predict: 512,
      num_ctx: 16384,
      mirostat: 0.0, //Maybe this one
      ignore_eos: false,
      stream: true,
    }
  }
}

#[derive(Deserialize, Debug)]
struct KoboldError{
  msg: String,
  r#type: String
}

#[derive(Deserialize)]
struct KoboldResponse{
  results: Vec<KoboldGeneration>,
}

#[derive(Deserialize)]
struct KoboldGeneration{
  text: String,
}

#[derive(Debug)]
pub struct StoredMessage{
  pub message: String,
  pub author: String,
}

const TEXT_START: &str = "<|begin_of_text|>";
const TEXT_END: &str = "<|eot_id|>";
const HEADER_START: &str = "<|start_header_id|>";
const HEADER_END: &str = "<|end_header_id|>";
const AI_DESC: &str = "You are a discord bot named Lily on a server called Big Gay Rock. You are speaking to the members of the server and will help them with whatever they ask.";

pub fn spawn_kobold_thread(origin_channel: u64, messages: Vec<StoredMessage>, message_storage_channel: UnboundedSender<StorageMessage>){
  tokio::spawn(async move{
    start_broadcasting(origin_channel).await;
    let bot_name = std::env::var("BOT_NAME").unwrap();
    let new_prompt = format!(
      "{TEXT_START}{HEADER_START}system{HEADER_END}\n\n{AI_DESC}{TEXT_END}"
    );
    let kobold_messages: Vec<String> = messages.iter().map(|msg| {
      format!("{HEADER_START}user{HEADER_END}\n\n{}: {}{TEXT_END}", msg.author, msg.message)
    }).collect();
    let context_messages = kobold_messages.join("");

    let ai_start = format!("{HEADER_START}assistant{HEADER_END}\n\n{bot_name}:");
    let prompt = new_prompt + &context_messages + &ai_start;
    
    let mut headers = header::HeaderMap::new();
    headers.insert("accept", header::HeaderValue::from_static("application/json"));
    headers.insert("Content-Type", header::HeaderValue::from_static("application/json"));
    let data = KoboldData::new(prompt);
    let client = match reqwest::Client::builder()
      .default_headers(headers)
      .timeout(Duration::from_secs(30))
      .build(){
        Ok(r) => r,
        Err(err) =>{
          println!("Reqwest client can't be built: {}", err);
          return;
        }
    };
    //add the / at the end of the server url if it's not there
    let mut kobold_server = std::env::var("KOBOLD_URL").unwrap();
    if kobold_server.chars().last().unwrap() != '/'{
      kobold_server.push('/');
    }
    kobold_server.push_str("api/v1/generate");
    let res = match client.post(kobold_server)
        .json(&data)
        .send()
        .await{
          Ok(r) => r,
          Err(err) => {
            println!("Unable to request from Kobold Server: {}", err);
            send_discord_message("The KoboldCPP server is down".to_string(), origin_channel).await;
            return;
          },
        };
    
    if res.status() == StatusCode::OK{
      let res_json: KoboldResponse = res.json().await.unwrap();
      for x in res_json.results.iter(){
        send_discord_message(x.text.clone(), origin_channel).await;
        if let Err(err) = message_storage_channel.send(StorageMessage{
          message: x.text.clone(),
          author: bot_name.to_string(),
          channel: origin_channel,
        }){
          println!("Unable to send kobold generation to storage thread: {}", err);
        }
      }
    }else if res.status() == StatusCode::SERVICE_UNAVAILABLE{
      let res_json: KoboldError = res.json().await.unwrap();
      println!("KoboldCPP Server is busy");
      println!("\tmsg: {}", res_json.msg);
      println!("\ttype: {}", res_json.r#type);
    }else{
      println!("Something weird happened: {:?}", res);
    }
  });
}
