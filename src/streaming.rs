//! Dictation-specific transport: reuse FlyOnTheWall's wire contract and normalizer,
//! but require end-of-stream metadata before committing, rather than a timed sleep.
use crate::transcript::Transcript;
use fotw_stt::{
    DeepgramEndpoint, DeepgramStreamParams, Source, SttError, SttErrorClass,
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
    audio: mpsc::Receiver<Vec<i16>>,
    endpoint: DeepgramEndpoint,
    deadline: Duration,
) -> Result<String, String> {
    transcribe_typed(key, audio, endpoint, deadline, deadline)
        .await
        .map_err(|e| e.message)
}

fn failure(class: SttErrorClass, message: &str) -> SttError {
    SttError::new(class, "deepgram", message)
}

/// Typed failures let fallback distinguish outages from invalid requests.
pub async fn transcribe_typed(
    key: String,
    audio: mpsc::Receiver<Vec<i16>>,
    endpoint: DeepgramEndpoint,
    connect_deadline: Duration,
    final_deadline: Duration,
) -> Result<String, SttError> {
    transcribe_with_vocabulary(key, audio, endpoint, connect_deadline, final_deadline, &[]).await
}

/// Send validated recognition hints only; snippets never enter provider prompts.
pub async fn transcribe_with_vocabulary(
    key: String,
    mut audio: mpsc::Receiver<Vec<i16>>,
    endpoint: DeepgramEndpoint,
    connect_deadline: Duration,
    final_deadline: Duration,
    hints: &[String],
) -> Result<String, SttError> {
    crate::vocabulary::validate_hints(hints)
        .map_err(|_| failure(SttErrorClass::BadRequest, "Invalid recognition hints"))?;
    if !endpoint.is_secure() && endpoint.host != "127.0.0.1" {
        return Err(failure(
            SttErrorClass::BadRequest,
            "Unencrypted remote provider rejected",
        ));
    }
    let params = DeepgramStreamParams::spec()
        .with_diarize(false)
        .with_keyterms(hints.iter().cloned());
    let mut request = endpoint
        .url_with(&params.to_query())
        .into_client_request()
        .map_err(|_| failure(SttErrorClass::BadRequest, "Invalid provider configuration"))?;
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Token {key}"))
            .map_err(|_| failure(SttErrorClass::Auth, "Invalid API key format"))?,
    );
    let (socket, _) = tokio::time::timeout(connect_deadline, connect_async(request))
        .await
        .map_err(|_| failure(SttErrorClass::Network, "Provider connection timed out"))?
        .map_err(|e| fotw_stt::map_transport_error(&e))?;
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
          Some(pcm)=>{sink.send(Message::binary(fotw_stt::to_linear16_le(&pcm))).await.map_err(|_|failure(SttErrorClass::Network,"Audio connection lost"))?;},
          None=>{sink.send(Message::text(fotw_stt::deepgram_wire::CLOSE_STREAM_FRAME)).await.map_err(|_|failure(SttErrorClass::Network,"Cannot finalize transcription"))?;finalizing=true;end.as_mut().reset(tokio::time::Instant::now()+final_deadline);}
         },
         message=incoming.next()=>match message {
          Some(Ok(Message::Text(raw)))=>{
           let wire:serde_json::Value=serde_json::from_str(raw.as_str()).map_err(|_|failure(SttErrorClass::BadRequest,"Invalid provider response"))?;
           if wire["type"]=="Error" {
            let error: fotw_stt::deepgram_wire::DeepgramErrorFrame = serde_json::from_value(wire).map_err(|_|failure(SttErrorClass::BadRequest,"Invalid provider error"))?;
            return Err(error.to_stt_error());
           }
           if wire["is_final"]==true && let Some(segment)=normalizer.push_json(raw.as_str()).map_err(|_|failure(SttErrorClass::BadRequest,"Invalid transcript response"))?{transcript.accept(&segment.id,segment.revision,&segment.text,segment.is_final);}
           if finalizing && wire["type"]=="Metadata" {
            if let Some(segment)=normalizer.finish(){transcript.accept(&segment.id,segment.revision,&segment.text,segment.is_final);}
            let _=sink.send(Message::Close(None)).await;
            return Ok(transcript.text());
           }
          },
          Some(Ok(Message::Ping(v)))=>{sink.send(Message::Pong(v)).await.map_err(|_|failure(SttErrorClass::Network,"Provider disconnected"))?;},
          Some(Ok(Message::Close(_)))|None|Some(Err(_))=>return Err(failure(SttErrorClass::Network,"Provider disconnected before finalization; no text inserted")),
          _=>{}
         },
         _=keepalive.tick()=>{sink.send(Message::text(fotw_stt::deepgram_wire::KEEPALIVE_FRAME)).await.map_err(|_|failure(SttErrorClass::Network,"Provider disconnected"))?;},
         _=&mut end,if finalizing=>return Err(failure(SttErrorClass::Network,"Finalization timed out; no text inserted"))
        }
    }
}
