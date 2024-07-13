use std::{borrow::BorrowMut, env, error::Error, sync::{Arc, Mutex}, time::{Duration, SystemTime, UNIX_EPOCH}};

use anyhow::anyhow;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use embedded_graphics::{pixelcolor::{BinaryColor, RgbColor}, prelude::{DrawTarget, Size}};
use m5stack_avatar_rs::{Avatar, components::{face::DrawContext, balloon::BalloonContext}, Palette, BasicPaletteKey, Timer, Expression};
use embedded_graphics_simulator::{SimulatorDisplay, Window, OutputSettingsBuilder, BinaryColorTheme, SimulatorEvent};
use nnnoiseless::{DenoiseState, RnnModel};
use openai_api_rs::v1::{api::OpenAIClient, assistant::AssistantRequest, audio::{self, AudioSpeechRequest, AudioTranscriptionRequest, TTS_1, WHISPER_1}, chat_completion::{self, ChatCompletionMessage, ChatCompletionRequest}, common::GPT4_O};
use rodio::{Decoder, Source};
struct StdTimer {}

mod framebuffer;
use crate::framebuffer::FbdevDisplay;

impl Timer for StdTimer {
    fn timestamp_milliseconds(&self) -> u64 {
        let now = SystemTime::now().duration_since(UNIX_EPOCH.into()).unwrap();
        let milliseconds = now.as_millis();
        milliseconds as u64
    }
}

fn sample_format(format: cpal::SampleFormat) -> hound::SampleFormat {
    if format.is_float() {
        hound::SampleFormat::Float
    } else {
        hound::SampleFormat::Int
    }
}

fn wav_spec_from_config(config: &cpal::SupportedStreamConfig) -> hound::WavSpec {
    hound::WavSpec {
        channels: config.channels() as _,
        sample_rate: config.sample_rate().0 as _,
        bits_per_sample: (config.sample_format().sample_size() * 8) as _,
        sample_format: sample_format(config.sample_format()),
        
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum VoiceDetectionState {
    Idle,
    Detected,
}

#[derive(Debug, Clone)]
struct VoiceDetectionRequest {
    path: String,
    timeout: Duration,
}

#[derive(Debug, Clone)]
struct AvatarUpdateRequest {
    expression: Option<Expression>,
    text: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let client = OpenAIClient::new(env::var("OPENAI_API_KEY").unwrap().to_string());

    let speech_output_path = "/tmp/stackchan_speech.mp3";

    let audio_host = cpal::default_host();
    let audio_devices = audio_host.output_devices().unwrap();
    for device in audio_devices {
        log::debug!("Audio device: {}", device.name().unwrap());
    }
    let output_device_name = std::env::var("ALSA_OUTPUT_DEVICE").unwrap();
    let input_device_name = std::env::var("ALSA_INPUT_DEVICE").unwrap();
    let audio_device = audio_host.output_devices().unwrap().find(|device| {
        device.name()
        .map(|name| name.contains(&output_device_name))
        .map_err(|_| false).unwrap_or(false)
    }).unwrap();
    let device_name = audio_device.name().unwrap();
    log::info!("Audio device: {}", device_name);
    let config = audio_device.default_output_config().unwrap();
    log::info!("Audio config: {:?}", config);
    
    let (_output_stream, stream_handle) = rodio::OutputStream::try_from_device(&audio_device).unwrap();
    let audio_devices = audio_host.input_devices().unwrap();
    for device in audio_devices {
        log::debug!("Audio input device: {}", device.name().unwrap());
    }
    let audio_input_device = audio_host.input_devices().unwrap().find(|device| {
        device.name()
        .map(|name| name.contains(&input_device_name))
        .map_err(|_| false).unwrap_or(false)
    }).unwrap();
    let audio_input_config: cpal::SupportedStreamConfig = audio_input_device.supported_input_configs().unwrap().filter(
        |config| config.sample_format() == cpal::SampleFormat::I16
    ).next().unwrap().with_sample_rate(cpal::SampleRate(48000));
    
    log::info!("Audio config: {:?}", &audio_input_config);
    let wavefile_spec = hound::WavSpec {
        channels: 1,
        sample_rate: 48000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    
    let err_fn = move |err| {
        log::error!("an error occurred on stream: {}", err);
    };

    const DENOISE_CHUNK_LENGTH: usize = DenoiseState::FRAME_SIZE;
    const VAD_CHUNK_LENGTH: usize = 48000*30/1000;
    let (audio_chunk_sender, mut audio_chunk_receiver) = tokio::sync::mpsc::channel::<[i16; DENOISE_CHUNK_LENGTH]>(2);
    let (voice_detection_request_sender, mut voice_detection_request_receiver) = tokio::sync::mpsc::channel::<VoiceDetectionRequest>(1);
    let (voice_detection_response_sender, mut voice_detection_response_receiver) = tokio::sync::mpsc::channel::<anyhow::Result<String>>(1);
    let (speak_request_sender, mut speak_request_receiver) = tokio::sync::mpsc::channel::<String>(2);
    let (avatar_update_request_sender, mut avatar_update_request_receiver) = tokio::sync::mpsc::channel::<AvatarUpdateRequest>(2);
    let (input_event_sender, mut input_event_receiver) = tokio::sync::mpsc::channel::<()>(2);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    {
        std::thread::spawn(move || {
            let local = tokio::task::LocalSet::new();
            local.spawn_local(async move  {
                let mut vad = webrtc_vad::Vad::new_with_rate_and_mode(webrtc_vad::SampleRate::Rate48kHz, webrtc_vad::VadMode::Aggressive);
                let mut writer = None;
                let mut number_of_chunks = 0;
                let mut number_of_inactive_chunks = 0;
                let mut vad_chunk = [0i16; VAD_CHUNK_LENGTH];
                let mut vad_chunk_filled = 0;
                let mut voice_detection_until: Option<std::time::Instant> = None;
                let mut output_path = None;
                let mut state = VoiceDetectionState::Idle;
                const DURATION_PER_CHUNK_MS: usize = 30;
                const END_OF_SPEECH_INACTIVE_CHUNKS: usize = 500 / DURATION_PER_CHUNK_MS;
                const MINIMUM_SPEECH_CHUNKS: usize = 500 / DURATION_PER_CHUNK_MS;

                while let Some(denoise_chunk) = audio_chunk_receiver.recv().await {
                    let mut denoise_chunk_offset = 0;
                    
                    match voice_detection_until.take() {
                        None => {
                            if !voice_detection_request_receiver.is_empty() {
                                if let Some(request) = voice_detection_request_receiver.recv().await {
                                    log::info!("Voice detection request.");
                                    voice_detection_until = Some(std::time::Instant::now() + request.timeout);
                                    denoise_chunk_offset = 0;
                                    output_path = Some(request.path);
                                }
                            }
                        },
                        Some(until) => {
                            if until < std::time::Instant::now() {
                                let _ = voice_detection_response_sender.send_timeout(Err(anyhow!("No voice input.")), Duration::from_millis(500)).await;
                            } else {
                                voice_detection_until = Some(until);
                            }
                        }
                    }

                    if voice_detection_until.is_none() {
                        continue;
                    }

                    while denoise_chunk_offset < denoise_chunk.len() {
                        let buffer_to_fill = (denoise_chunk.len() - denoise_chunk_offset).min(vad_chunk.len() - vad_chunk_filled);
                        vad_chunk[vad_chunk_filled..vad_chunk_filled + buffer_to_fill].copy_from_slice(&denoise_chunk[denoise_chunk_offset..denoise_chunk_offset + buffer_to_fill]);
                        denoise_chunk_offset += buffer_to_fill;
                        vad_chunk_filled += buffer_to_fill;

                        if vad_chunk_filled == vad_chunk.len() {
                            vad_chunk_filled = 0;
                            let new_state = match &state {
                                VoiceDetectionState::Idle => {
                                    let is_speech = vad.is_voice_segment(&vad_chunk).unwrap();
                                    if is_speech {
                                        if let Some(path) = &output_path {
                                            let mut new_writer = hound::WavWriter::create(&path, wavefile_spec).unwrap();
                                            log::info!("Speech detected");
                                            for &sample in vad_chunk.iter() {
                                                new_writer.write_sample(sample).ok();
                                            }
                                            writer = Some(new_writer);
                                            number_of_chunks = 1;
                                            number_of_inactive_chunks = 0;
                                            VoiceDetectionState::Detected
                                        } else {
                                            VoiceDetectionState::Idle
                                        }
                                    } else {
                                        VoiceDetectionState::Idle
                                    }
                                },
                                VoiceDetectionState::Detected => {
                                    number_of_chunks += 1;
                                    for &sample in vad_chunk.iter() {
                                        writer.as_mut().unwrap().write_sample(sample).ok();
                                    }
        
                                    let is_speech = vad.is_voice_segment(&vad_chunk).unwrap();
                                    if is_speech {
                                        number_of_inactive_chunks = 0;
                                    } else {
                                        number_of_inactive_chunks += 1;
                                    }
        
                                    if number_of_inactive_chunks > END_OF_SPEECH_INACTIVE_CHUNKS {
                                        log::info!("End of speech. detected chunks: {}", number_of_chunks);
                                        writer = None;
                                        if number_of_chunks > MINIMUM_SPEECH_CHUNKS && voice_detection_until.is_some() {
                                            voice_detection_until = None;
                                            let _ = voice_detection_response_sender.send_timeout(Ok(output_path.take().unwrap()), Duration::from_millis(500)).await;
                                        }
                                        VoiceDetectionState::Idle
                                    } else {
                                        VoiceDetectionState::Detected
                                    }
                                },
                            };
                            state = new_state;
                        }
                    }
                }
            });
            rt.block_on(local);
        });
    }

    let lipsync_average = Arc::new(std::sync::atomic::AtomicI16::new(0));

    struct LipSyncSource<Source> {
        source: Source,
        average: Arc<std::sync::atomic::AtomicI16>,
    }

    impl<S> LipSyncSource<S> 
        where S: rodio::Source<Item = i16> {
        fn new(source: S, average: Arc<std::sync::atomic::AtomicI16>) -> Self {
            Self {
                source,
                average,
            }
        }
    }

    impl<S> Iterator for LipSyncSource<S> 
        where S: rodio::Source<Item = i16> {
        type Item = i16;
        fn next(&mut self) -> Option<Self::Item> {
            self.source.next().map(|sample| {
                let average = self.average.load(std::sync::atomic::Ordering::Relaxed) as i32;
                let sample = sample as i32;
                let sample = sample + (sample - average) / 2;
                self.average.store(sample.max(i16::MIN as i32).min(i16::MAX as i32) as i16, std::sync::atomic::Ordering::Relaxed);
                sample as i16
            })
        }
    }

    impl<S> Source for LipSyncSource<S> 
        where S: rodio::Source<Item = i16> {
        fn channels(&self) -> u16 {
            self.source.channels()
        }
        fn current_frame_len(&self) -> Option<usize> {
            self.source.current_frame_len()
        }
        fn sample_rate(&self) -> u32 {
            self.source.sample_rate()
        }
        fn total_duration(&self) -> Option<Duration> {
            self.source.total_duration()
        }
    }

    // Speaker process
    {
        let lipsync_average = lipsync_average.clone();
        tokio::spawn(async move {
            let mut speak_sink: Option<rodio::Sink> = None;
            while let Some(content) = speak_request_receiver.recv().await {
                if content.is_empty() {
                    if let Some(sink) = speak_sink.take() {
                        sink.stop();
                    }
                    continue;
                }
                let req = AudioSpeechRequest::new(
                    TTS_1.to_string(),
                    content,
                    audio::VOICE_ALLOY.to_string(),
                    String::from(speech_output_path),
                );
                let result = client.audio_speech(req).await;
                match result {
                    Ok(_response) => {
                        let file = std::io::BufReader::new(std::fs::File::open(speech_output_path).unwrap());
                        let source = Decoder::new(file).unwrap();
                        //stream_handle.play_raw(source.convert_samples()).unwrap();
                        if let Some(sink) = speak_sink.take() {
                            sink.stop();
                        }
                        let sink = rodio::Sink::try_new(&stream_handle).unwrap();
                        sink.append(LipSyncSource::new(source, lipsync_average.clone()));
                        sink.play();
                        speak_sink = Some(sink);
                    },
                    Err(err) => {
                        log::error!("Failed to speak: {:?}", err);
                    }
                }
            }
        });
    }

    // Audio Sampling and denoise filter process.
    // Fill the denoise buffer (whose size must be equal to the DenoiseState::FRAME_SIZE) with the input audio samples.
    // Then the denoise state processes the frame and the denoised frame is sent to the speech recognition process.
    let mut denoise_buffer_filled = 0;
    let mut denoise_chunk_buffer = [0f32; DenoiseState::FRAME_SIZE];
    let mut denoised_chunk_buffer = [0f32; DenoiseState::FRAME_SIZE];
    let mut denoised_chunk_buffer_i16 = [0; DenoiseState::FRAME_SIZE];
    let mut denoise_state = DenoiseState::new();
    let input_stream = audio_input_device.build_input_stream(
        &audio_input_config.into(), 
        move |input: &[i16], _| {
            let mut offset = 0;
            while input.len() - offset > 0 {
                let buffer_to_fill = (input.len() - offset).min(denoise_chunk_buffer.len() - denoise_buffer_filled);
                for index in 0..buffer_to_fill {
                    denoise_chunk_buffer[denoise_buffer_filled + index] = input[offset + index] as f32;
                }
                denoise_buffer_filled += buffer_to_fill;
                offset += buffer_to_fill;
                if denoise_buffer_filled == denoise_chunk_buffer.len() {
                    denoise_state.process_frame(&mut denoised_chunk_buffer, &denoise_chunk_buffer);
                    for index in 0..denoised_chunk_buffer.len() {
                        denoised_chunk_buffer_i16[index] = denoised_chunk_buffer[index] as i16;
                    }
                    let _ = audio_chunk_sender.try_send(denoised_chunk_buffer_i16);
                    denoise_buffer_filled = 0;
                }
            }
        }, 
        err_fn, 
        None
    ).unwrap();
    input_stream.play().unwrap();

    //
    // Display process
    //
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    std::thread::spawn(move || {
        let local = tokio::task::LocalSet::new();
        local.spawn_local(async move  {
            #[cfg(not(feature="framebuffer"))]
            let (mut display, context, clear_color ) = {
                let display = SimulatorDisplay::<BinaryColor>::new(Size::new(320, 240));
                let mut context: DrawContext<BinaryColor, String> = DrawContext::default();
                context.palette.set_color(&BasicPaletteKey::Primary, BinaryColor::On);
                context.palette.set_color(&BasicPaletteKey::Secondary, BinaryColor::On);
                context.palette.set_color(&BasicPaletteKey::Background, BinaryColor::Off);
                context.palette.set_color(&BasicPaletteKey::BalloonForeground, BinaryColor::On);
                context.palette.set_color(&BasicPaletteKey::BalloonBackground, BinaryColor::Off);
                context.set_text(None);
                (display, context, BinaryColor::Off)
            };

            #[cfg(feature="framebuffer")]
            let (mut display, context, clear_color) = {
                let display = FbdevDisplay::new(&env::var("FBDEV_PATH").unwrap());
                let mut context: DrawContext<embedded_graphics::pixelcolor::Rgb565, String> = DrawContext::default();
                context.palette.set_color(&BasicPaletteKey::Primary, embedded_graphics::pixelcolor::Rgb565::WHITE);
                context.palette.set_color(&BasicPaletteKey::Secondary, embedded_graphics::pixelcolor::Rgb565::WHITE);
                context.palette.set_color(&BasicPaletteKey::Background, embedded_graphics::pixelcolor::Rgb565::BLACK);
                context.palette.set_color(&BasicPaletteKey::BalloonForeground, embedded_graphics::pixelcolor::Rgb565::WHITE);
                context.palette.set_color(&BasicPaletteKey::BalloonBackground, embedded_graphics::pixelcolor::Rgb565::BLACK);
                context.set_text(None);
                (display, context, embedded_graphics::pixelcolor::Rgb565::BLACK)
            };
            
            let mut avatar = Avatar::new(context, 30);
            let timer = StdTimer{};
            
            #[cfg(not(feature="framebuffer"))]
            let output_settings = OutputSettingsBuilder::new()
                .theme(BinaryColorTheme::OledBlue)
                .build();
            #[cfg(not(feature="framebuffer"))]
            let mut window = Window::new("Avatar", &output_settings);

            'main_loop: loop {
                let next_time = tokio::time::Instant::now() + Duration::from_millis(1000/30);
                display.clear(clear_color).ok();

                if !avatar_update_request_receiver.is_empty() {
                    if let Some(request) = avatar_update_request_receiver.recv().await {
                        if let Some(expression) = &request.expression {
                            avatar.context().expression = *expression;
                        }
                        if let Some(text) = &request.text {
                            avatar.context().set_text(if text.is_empty() { None } else { Some(text) });
                        }
                    }
                }
                
                let speaker_average_output = lipsync_average.load(std::sync::atomic::Ordering::Relaxed);
                let output_amplitude = (speaker_average_output as f32).abs() / i16::MAX as f32;
                avatar.context().mouth_open_ratio = (output_amplitude * 4.0).min(1.0);
                avatar.run(&mut display, &timer).ok();
                
                #[cfg(not(feature="framebuffer"))]
                {
                    window.update(&display);
                    for event in window.events() {
                        match event {
                            SimulatorEvent::Quit => break 'main_loop,
                            SimulatorEvent::MouseButtonDown { mouse_btn, point } => {
                                log::info!("MouseButtonDown: {:?}, {:?}", mouse_btn, point);
                                let _mouse_btn = mouse_btn;
                                let _point = point;
                                input_event_sender.try_send(()).ok();
                            },
                            _ => {}
                        }
                    
                    }
                }
                #[cfg(feature="framebuffer")]
                {
                    display.update();
                }
                tokio::time::sleep_until(next_time).await;
            }
        });
        rt.block_on(local);
    });

    // input process
    #[cfg(feature="framebuffer")]
    tokio::spawn(async move {
        let input_device = evdev::Device::open("/dev/input/event0").unwrap();
        let mut events = input_device.into_event_stream().unwrap();
        loop {
            let ev = events.next_event().await;
            let ev = match ev  {
                Ok(ev) => ev,
                Err(_err) => {
                    continue;
                }
            };
            
            match ev.event_type() {
                evdev::EventType::ABSOLUTE => {
                    if ev.code() == evdev::AbsoluteAxisType::ABS_MT_POSITION_Y.0 {
                        input_event_sender.try_send(()).ok();
                    }
                },
                _ => {},
            }
        }
    });

    // Main process
    let client = OpenAIClient::new(env::var("OPENAI_API_KEY").unwrap().to_string());
    let mut completion_messages = vec![
        ChatCompletionMessage {
            role: chat_completion::MessageRole::system,
            content: chat_completion::Content::Text("以降のユーザーからの質問に対して、200字までの内容で回答してください。".into()),
            name: None,
        }
    ];
    loop {
        // Wait input
        let _ = input_event_receiver.recv().await;

        let _ = speak_request_sender.send_timeout("".into(), Duration::from_millis(1000)).await;

        let _ = avatar_update_request_sender.send(AvatarUpdateRequest {
            expression: Some(Expression::Neutral),
            text: Some("Listening...".into()),
        }).await;

        // Voice detection
        let request = VoiceDetectionRequest {
            path: "/tmp/input_voice.wav".into(),
            timeout: Duration::from_secs(10),
        };
        match voice_detection_request_sender.send_timeout(request, Duration::from_millis(500)).await {
            Ok(()) => {},
            Err(_) => { continue; },
        }
        let response = voice_detection_response_receiver.recv().await.unwrap();
        let path = match response {
            Ok(path) => {
                path
            },
            Err(_) => { continue; },
        };

        let _ = avatar_update_request_sender.send(AvatarUpdateRequest {
            expression: Some(Expression::Neutral),
            text: Some("Recognizing...".into()),
        }).await;

        // Voice recongnition.
        // Post the recorded speech data to OpenAI
        let request = AudioTranscriptionRequest::new(
            path,
            WHISPER_1.into(),
        ).language("ja".into());

        let result = client.audio_transcription(request).await;
        let text = match result {
            Ok(response) => {
                log::info!("Transcription: {}", response.text);
                response.text
            },
            Err(err) => {
                log::error!("Failed to transcribe audio: {:?}", err);
                continue;
            },
        };

        // Chat completion
        // Post the transcription to Chat.

        let _ = avatar_update_request_sender.send(AvatarUpdateRequest {
            expression: Some(Expression::Neutral),
            text: Some("Thinking...".into()),
        }).await;

        completion_messages.push(ChatCompletionMessage {
            role: chat_completion::MessageRole::user,
            content: chat_completion::Content::Text(text.clone()),
            name: None,
        });
        let request = ChatCompletionRequest::new(
            GPT4_O.into(),
            completion_messages.clone(),
        );
        let result = client.chat_completion(request).await;
        match result {
            Ok(response) => {
                log::info!("Chat completion: {:?}", response);
                let _ = avatar_update_request_sender.send(AvatarUpdateRequest {
                    expression: Some(Expression::Happy),
                    text: Some("".into()),
                }).await;

                let message = &response.choices[0].message;
                if let Some(content) = &message.content {
                    completion_messages.push(ChatCompletionMessage {
                        role: message.role.clone(),
                        content: chat_completion::Content::Text(content.clone()),
                        name: None,
                    });
                    // Post the completion to the speaker.
                    let _ = speak_request_sender.send_timeout(content.clone(), Duration::from_millis(1000)).await;
                }
            },
            Err(err) => {
                log::error!("Failed to chat completion: {:?}", err);
                let _ = speak_request_sender.send_timeout("エラーが発生しました。".into(), Duration::from_millis(1000)).await;
                let _ = avatar_update_request_sender.send(AvatarUpdateRequest {
                    expression: Some(Expression::Sad),
                    text: Some("Error!".into()),
                }).await;
            }
        }
    }   
}