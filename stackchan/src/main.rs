use std::{borrow::BorrowMut, env, error::Error, sync::{Arc, Mutex}, time::{Duration, SystemTime, UNIX_EPOCH}};

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
enum SpeechRecognitionState {
    Idle,
    Detected,
    Recognizing,
    Thinking,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let client = OpenAIClient::new(env::var("OPENAI_API_KEY").unwrap().to_string());

    let speech_output_path = "/tmp/stackchan_speech.mp3";
    // let req = AudioSpeechRequest::new(
    //     TTS_1.to_string(),
    //     String::from("こんにちは。私はｽﾀｯｸﾁｬﾝです。よろしくお願いします。"),
    //     audio::VOICE_ALLOY.to_string(),
    //     String::from(speech_output_path),
    // );

    // let result = client.audio_speech(req).await?;
    // log::info!("{:?}", result);

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
    
    let (output_stream, stream_handle) = rodio::OutputStream::try_from_device(&audio_device).unwrap();
    //let file = std::io::BufReader::new(std::fs::File::open(speech_output_path).unwrap());
    // Decode that sound file into a source
    //let source = Decoder::new(file).unwrap();
    // Play the sound directly on the device
    //stream_handle.play_raw(source.convert_samples()).unwrap();

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
    let speech_recognition_state = Arc::new(tokio::sync::Mutex::new(SpeechRecognitionState::Idle));
    let (audio_chunk_sender, mut audio_chunk_receiver) = tokio::sync::mpsc::channel::<[i16; DENOISE_CHUNK_LENGTH]>(2);
    let (speak_request_sender, mut speak_request_receiver) = tokio::sync::mpsc::channel::<String>(2);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    {
        let speech_recognition_state = speech_recognition_state.clone();
        std::thread::spawn(move || {
            let local = tokio::task::LocalSet::new();
            local.spawn_local(async move  {
                const PATH: &str = "/tmp/speech.wav";
                let client = OpenAIClient::new(env::var("OPENAI_API_KEY").unwrap().to_string());
                let mut vad = webrtc_vad::Vad::new_with_rate_and_mode(webrtc_vad::SampleRate::Rate48kHz, webrtc_vad::VadMode::Aggressive);
                let mut writer = None;
                let mut number_of_chunks = 0;
                let mut number_of_inactive_chunks = 0;
                let mut vad_chunk = [0i16; VAD_CHUNK_LENGTH];
                let mut vad_chunk_filled = 0;
                let mut completion_messages = vec![];
                
                const DURATION_PER_CHUNK_MS: usize = 30;
                const END_OF_SPEECH_INACTIVE_CHUNKS: usize = 500 / DURATION_PER_CHUNK_MS;
                const MINIMUM_SPEECH_CHUNKS: usize = 500 / DURATION_PER_CHUNK_MS;

                while let Some(denoise_chunk) = audio_chunk_receiver.recv().await {
                    let mut denoise_chunk_offset = 0;
                    while denoise_chunk_offset < denoise_chunk.len() {
                        let buffer_to_fill = (denoise_chunk.len() - denoise_chunk_offset).min(vad_chunk.len() - vad_chunk_filled);
                        vad_chunk[vad_chunk_filled..vad_chunk_filled + buffer_to_fill].copy_from_slice(&denoise_chunk[denoise_chunk_offset..denoise_chunk_offset + buffer_to_fill]);
                        denoise_chunk_offset += buffer_to_fill;
                        vad_chunk_filled += buffer_to_fill;
                        if vad_chunk_filled == vad_chunk.len() {
                            vad_chunk_filled = 0;
                            let new_state = match speech_recognition_state.lock().await.clone() {
                                SpeechRecognitionState::Idle => {
                                    let is_speech = vad.is_voice_segment(&vad_chunk).unwrap();
                                    if is_speech {
                                        let mut new_writer = hound::WavWriter::create(PATH, wavefile_spec).unwrap();
                                        log::info!("Speech detected");
                                        for &sample in vad_chunk.iter() {
                                            new_writer.write_sample(sample).ok();
                                        }
                                        writer = Some(new_writer);
                                        number_of_chunks = 1;
                                        number_of_inactive_chunks = 0;
                                        SpeechRecognitionState::Detected
                                    } else {
                                        SpeechRecognitionState::Idle
                                    }
                                },
                                SpeechRecognitionState::Detected => {
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
                                        log::info!("End of speech detected chunks: {}", number_of_chunks);
                                        writer = None;
                                        if number_of_chunks > MINIMUM_SPEECH_CHUNKS {
                                            SpeechRecognitionState::Recognizing
                                        } else {
                                            SpeechRecognitionState::Idle
                                        }
                                    } else {
                                        SpeechRecognitionState::Detected
                                    }
                                },
                                SpeechRecognitionState::Recognizing => {
                                    // Post the recorded speech data to OpenAI
                                    let request = AudioTranscriptionRequest::new(
                                        PATH.into(),
                                        WHISPER_1.into(),
                                    ).language("ja".into());
        
                                    let result = client.audio_transcription(request).await;
                                    match result {
                                        Ok(response) => {
                                            log::info!("Transcription: {}", response.text);
                                            completion_messages.push(ChatCompletionMessage {
                                                role: chat_completion::MessageRole::user,
                                                content: chat_completion::Content::Text(response.text),
                                                name: None,
                                            });
                                            SpeechRecognitionState::Thinking
                                        },
                                        Err(err) => {
                                            log::error!("Failed to transcribe audio: {:?}", err);
                                            SpeechRecognitionState::Idle
                                        }
                                    }
                                },
                                SpeechRecognitionState::Thinking => {
                                    // Post the transcription to Chat.
                                    let request = ChatCompletionRequest::new(
                                        GPT4_O.into(),
                                        completion_messages.clone(),
                                    );
                                    let result = client.chat_completion(request).await;
                                    match result {
                                        Ok(response) => {
                                            log::info!("Chat completion: {:?}", response);
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
                                            SpeechRecognitionState::Idle
                                        },
                                        Err(err) => {
                                            log::error!("Failed to chat completion: {:?}", err);
                                            SpeechRecognitionState::Idle
                                        }
                                    }

                                },
                            };
                            *speech_recognition_state.lock().await = new_state;       
                        }
                    }
                }
            });
            rt.block_on(local);
        });
    }

    // Speaker process
    tokio::spawn(async move {
        let mut speak_sink: Option<rodio::Sink> = None;
        while let Some(content) = speak_request_receiver.recv().await {
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
                    sink.append(source);
                    sink.play();
                    speak_sink = Some(sink);
                },
                Err(err) => {
                    log::error!("Failed to speak: {:?}", err);
                }
            }
        }
    });

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

    #[cfg(not(feature="framebuffer"))]
    let (mut display, context, clear_color ) = {
        let display = SimulatorDisplay::<BinaryColor>::new(Size::new(320, 240));
        let mut context: DrawContext<BinaryColor, String> = DrawContext::default();
        context.palette.set_color(&BasicPaletteKey::Primary, BinaryColor::On);
        context.palette.set_color(&BasicPaletteKey::Secondary, BinaryColor::On);
        context.palette.set_color(&BasicPaletteKey::Background, BinaryColor::Off);
        context.palette.set_color(&BasicPaletteKey::BalloonForeground, BinaryColor::On);
        context.palette.set_color(&BasicPaletteKey::BalloonBackground, BinaryColor::Off);
        context.set_text(Some("ほげふがぴよ"));
        (display, context, BinaryColor::Off)
    };

    #[cfg(feature="framebuffer")]
    let (mut display, context, clear_color ) = {
        let display = FbdevDisplay::new(&env::var("FBDEV_PATH").unwrap());
        let mut context: DrawContext<embedded_graphics::pixelcolor::Rgb565, String> = DrawContext::default();
        context.palette.set_color(&BasicPaletteKey::Primary, embedded_graphics::pixelcolor::Rgb565::WHITE);
        context.palette.set_color(&BasicPaletteKey::Secondary, embedded_graphics::pixelcolor::Rgb565::WHITE);
        context.palette.set_color(&BasicPaletteKey::Background, embedded_graphics::pixelcolor::Rgb565::BLACK);
        context.palette.set_color(&BasicPaletteKey::BalloonForeground, embedded_graphics::pixelcolor::Rgb565::WHITE);
        context.palette.set_color(&BasicPaletteKey::BalloonBackground, embedded_graphics::pixelcolor::Rgb565::BLACK);
        context.set_text(Some("ほげふがぴよ"));
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

    loop {
        display.clear(clear_color)?;

        match speech_recognition_state.lock().await.clone() {
            SpeechRecognitionState::Idle => {
                avatar.context().set_text(None);
                avatar.context().expression = Expression::Sleepy;
            },
            SpeechRecognitionState::Detected => {
                avatar.context().set_text(Some("Listening..."));
                avatar.context().expression = Expression::Neutral;
            },
            SpeechRecognitionState::Recognizing => {
                avatar.context().set_text(Some("Thinking..."));
                avatar.context().expression = Expression::Neutral;
            },
            SpeechRecognitionState::Thinking => {
                avatar.context().set_text(Some("Thinking..."));
                avatar.context().expression = Expression::Neutral;
            },
        }
        avatar.context().mouth_open_ratio = 0.5;
        avatar.run(&mut display, &timer)?;
        
        #[cfg(not(feature="framebuffer"))]
        {
            window.update(&display);
            if window.events().any(|e| e == SimulatorEvent::Quit) {
                break;
            }
        }
        #[cfg(feature="framebuffer")]
        {
            display.update();
        }
        tokio::time::sleep(Duration::from_millis(1000/30)).await;
    }

    Ok(())
}

// fn make_sine_wave_stream(audio_device: &cpal::Device, config: &cpal::SupportedStreamConfig, frequency_hz: f32) -> cpal::Stream {
//     let mut counter = 0usize;
//     let sample_rate = config.sample_rate().0;
//     let number_of_channels = config.channels() as usize;
//     let err_fn = |err| log::error!("an error occurred on the output audio stream: {}", err);
//     let stream = audio_device.build_output_stream(
//         &config.clone().into(),
//         move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
//             let sampls_to_generate = data.len() / number_of_channels;
//             for index in 0..sampls_to_generate {
//                 let sample = (counter as f32 * frequency_hz * 2.0 * std::f32::consts::PI / sample_rate as f32).sin();
//                 for channel in 0..number_of_channels {
//                     data[index * number_of_channels + channel] = sample;
//                 }
//                 counter = counter.wrapping_add(1);
//             }
//         },
//         err_fn, 
//         None).unwrap();
//     stream
// }