//! Dictation-specific transport: reuse FlyOnTheWall's wire contract and normalizer,
//! but require end-of-stream metadata before committing, rather than a timed sleep.
use crate::transcript::Transcript;
use fotw_stt::{
    DeepgramEndpoint, DeepgramStreamParams, Source,
    deepgram::{DeepgramConfig, DeepgramNormalizer},
};
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        Message,
        client::IntoClientRequest,
        http::{HeaderValue, header::AUTHORIZATION},
    },
};
pub async fn transcribe(
    key: String,
    mut audio: mpsc::Receiver<Vec<i16>>,
    endpoint: DeepgramEndpoint,
    deadline: Duration,
) -> Result<String, String> {
    if !endpoint.is_secure() && endpoint.host != "127.0.0.1" {
        return Err("Unencrypted remote provider rejected".into());
    }
    let params = DeepgramStreamParams::spec().with_diarize(false);
    let mut request = endpoint
        .url_with(&params.to_query())
        .into_client_request()
        .map_err(|_| "Invalid provider configuration")?;
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Token {key}")).map_err(|_| "Invalid API key format")?,
    );
    let (socket, _) = tokio::time::timeout(deadline, connect_async(request))
        .await
        .map_err(|_| "Provider connection timed out")?
        .map_err(|_| "Cannot connect to Deepgram; check key and network")?;
    let (mut sink, mut incoming) = socket.split();
    let mut normalizer = DeepgramNormalizer::new(DeepgramConfig::new("dictation", Source::Mic));
    let mut transcript = Transcript::default();
    let mut finalizing = false;
    let end = tokio::time::sleep(Duration::from_secs(24 * 60 * 60));
    tokio::pin!(end);
    let mut keepalive = tokio::time::interval(Duration::from_secs(4));
    keepalive.tick().await;
    loop {
        tokio::select! {
         chunk=audio.recv(),if !finalizing=>match chunk {
          Some(pcm)=>{sink.send(Message::binary(fotw_stt::to_linear16_le(&pcm))).await.map_err(|_|"Audio connection lost")?;},
          None=>{sink.send(Message::text(fotw_stt::deepgram_wire::CLOSE_STREAM_FRAME)).await.map_err(|_|"Cannot finalize transcription")?;finalizing=true;end.as_mut().reset(tokio::time::Instant::now()+deadline);}
         },
         message=incoming.next()=>match message {
          Some(Ok(Message::Text(raw)))=>{
           let wire:serde_json::Value=serde_json::from_str(raw.as_str()).map_err(|_|"Invalid provider response")?;
           if wire["type"]=="Error" {return Err("Provider rejected transcription".into());}
           if wire["is_final"]==true && let Some(segment)=normalizer.push_json(raw.as_str()).map_err(|_|"Invalid transcript response")?{transcript.accept(&segment.id,segment.revision,&segment.text,segment.is_final);}
           if finalizing && wire["type"]=="Metadata" {
            if let Some(segment)=normalizer.finish(){transcript.accept(&segment.id,segment.revision,&segment.text,segment.is_final);}
            let _=sink.send(Message::Close(None)).await;
            return Ok(transcript.text());
           }
          },
          Some(Ok(Message::Ping(v)))=>{sink.send(Message::Pong(v)).await.map_err(|_|"Provider disconnected")?;},
          Some(Ok(Message::Close(_)))|None|Some(Err(_))=>return Err("Provider disconnected before finalization; no text inserted".into()),
          _=>{}
         },
         _=keepalive.tick()=>{sink.send(Message::text(fotw_stt::deepgram_wire::KEEPALIVE_FRAME)).await.map_err(|_|"Provider disconnected")?;},
         _=&mut end,if finalizing=>return Err("Finalization timed out; no text inserted".into())
        }
    }
}
