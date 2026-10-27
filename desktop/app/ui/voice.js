// Dictation: the microphone button writes what the person says into the message box (it does not send). The words are recognised on this computer by
// an on-device model that comes down once, on the first launch: free, private, and the same on the Mac and on Windows.
(() => {
  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;
  const $ = (id) => document.getElementById(id);
  const button = $("voice-button");
  const input = $("chat-input");

  let take = null;
  const START = 0.018;   // a sound that counts as speech

  function toWav(frames, rate) {
    const length = frames.reduce((sum, frame) => sum + frame.length, 0);
    const merged = new Float32Array(length);
    let offset = 0;
    for (const frame of frames) { merged.set(frame, offset); offset += frame.length; }
    const factor = Math.max(1, Math.round(rate / 16000));
    const samples = Math.floor(length / factor);
    const buffer = new ArrayBuffer(44 + samples * 2);
    const view = new DataView(buffer);
    const text = (at, value) => { for (let index = 0; index < value.length; index++) view.setUint8(at + index, value.charCodeAt(index)); };
    text(0, "RIFF"); view.setUint32(4, 36 + samples * 2, true); text(8, "WAVEfmt ");
    view.setUint32(16, 16, true); view.setUint16(20, 1, true); view.setUint16(22, 1, true);
    view.setUint32(24, rate / factor, true); view.setUint32(28, (rate / factor) * 2, true); view.setUint16(32, 2, true); view.setUint16(34, 16, true);
    text(36, "data"); view.setUint32(40, samples * 2, true);
    for (let index = 0; index < samples; index++) {
      let sum = 0;
      for (let step = 0; step < factor; step++) sum += merged[index * factor + step];
      view.setInt16(44 + index * 2, Math.max(-1, Math.min(1, sum / factor)) * 0x7fff, true);
    }
    return new Uint8Array(buffer);
  }

  function base64Of(bytes) {
    let binary = "";
    for (let index = 0; index < bytes.length; index += 0x8000) binary += String.fromCharCode(...bytes.subarray(index, index + 0x8000));
    return btoa(binary);
  }

  function insert(words) {
    const start = input.selectionStart ?? input.value.length;
    const end = input.selectionEnd ?? input.value.length;
    const before = input.value.slice(0, start);
    const glue = before && !/\s$/.test(before) ? " " : "";
    input.value = `${before}${glue}${words}${input.value.slice(end)}`;
    const caret = before.length + glue.length + words.length;
    input.setSelectionRange(caret, caret);
    input.dispatchEvent(new Event("input"));
    input.focus();
  }

  // The dictation keeps listening: every pause (0.8 s) closes a phrase, which is recognised and written at once while the next one is already being said.
  // It ends when the button is pressed again, with Esc, or after six quiet seconds.
  const PHRASE_PAUSE = 800;
  const IDLE_END = 6000;
  let queue = Promise.resolve();
  let pendingPhrases = 0;

  function writePhrase(frames, rate) {
    pendingPhrases += 1;
    button.classList.add("busy");
    queue = queue.then(async () => {
      try {
        const words = ((await invoke("listen_transcribe", { audio: base64Of(toWav(frames, rate)), language: (navigator.language || "tr").slice(0, 2).toLowerCase() })) || "").trim();
        if (words) insert(words);
      } catch (error) {
        window.toast?.(String(error), "bad");
      } finally {
        pendingPhrases -= 1;
        if (pendingPhrases === 0) button.classList.remove("busy");
      }
    });
  }

  async function begin() {
    const status = await invoke("voice_model_status").catch(() => null);
    if (status && status.state !== "ready") {
      window.toast?.(status.state === "downloading" ? `Ses paketi iniyor (%${Math.round(status.progress * 100)}); bitince yazdırabilirsin.` : (status.message || "Ses paketi henüz yok."), "bad");
      if (status.state === "failed" || status.state === "missing") invoke("voice_model_retry");
      return;
    }
    let stream;
    try {
      stream = await navigator.mediaDevices.getUserMedia({ audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true } });
    } catch (error) {
      window.toast?.(`Mikrofon açılamadı (${error?.name || "izin"}). Sistem ayarlarından Wiqo'ya mikrofon izni ver.`, "bad");
      return;
    }
    const context = new AudioContext();
    const source = context.createMediaStreamSource(stream);
    const processor = context.createScriptProcessor(2048, 1, 1);
    const quiet = context.createGain();
    quiet.gain.value = 0;
    source.connect(processor);
    processor.connect(quiet).connect(context.destination);
    take = { stream, context, source, processor, preRoll: [], frames: [], speechAt: 0, quietAt: 0, lastSoundAt: Date.now(), cancelled: false };
    button.classList.add("on");
    processor.onaudioprocess = (event) => {
      if (!take) return;
      const frame = new Float32Array(event.inputBuffer.getChannelData(0));
      const level = Math.sqrt(frame.reduce((sum, value) => sum + value * value, 0) / frame.length);
      button.style.setProperty("--level", String(Math.min(1, level * 9)));
      const now = Date.now();
      if (level > START) take.lastSoundAt = now;
      if (!take.speechAt) {
        take.preRoll.push(frame);
        if (take.preRoll.length > 8) take.preRoll.shift();
        if (level > START) { take.speechAt = now; take.frames.push(...take.preRoll); take.preRoll = []; }
        else if (now - take.lastSoundAt > IDLE_END) finish();
        return;
      }
      take.frames.push(frame);
      if (level > START * 0.7) take.quietAt = 0;
      else {
        take.quietAt ||= now;
        if (now - take.quietAt > PHRASE_PAUSE || now - take.speechAt > 25_000) {
          // A phrase is done: it goes to be written, and the next one starts from the next sound.
          if (now - take.speechAt > 450) writePhrase(take.frames, take.context.sampleRate);
          take.frames = [];
          take.speechAt = 0;
          take.quietAt = 0;
        }
      }
    };
  }

  function finish() {
    const done = take;
    if (!done) return;
    take = null;
    done.processor.onaudioprocess = null;
    done.processor.disconnect();
    done.source.disconnect();
    done.stream.getTracks().forEach((track) => track.stop());
    button.classList.remove("on");
    button.style.removeProperty("--level");
    // What was being said when the button was pressed is still written.
    if (!done.cancelled && done.speechAt && Date.now() - done.speechAt > 450) writePhrase(done.frames, done.context.sampleRate);
    setTimeout(() => done.context.close(), 100);
  }

  button.onclick = () => (take ? finish() : begin());
  document.addEventListener("keydown", (event) => { if (event.key === "Escape" && take) { take.cancelled = true; finish(); } });

  // ----- the speech model's one-time download -----
  const modelBox = $("voice-model");
  function showModel(status) {
    modelBox.hidden = status.state === "ready" || status.state === "missing";
    if (modelBox.hidden) return;
    if (status.state === "downloading") {
      modelBox.onclick = null;
      modelBox.innerHTML = `Ses paketi indiriliyor %${Math.round(status.progress * 100)}<div class="bar"><i style="width:${Math.round(status.progress * 100)}%"></i></div>`;
    } else {
      modelBox.textContent = `${status.message || "Ses paketi indirilemedi."} Yeniden denemek için dokun.`;
      modelBox.onclick = () => invoke("voice_model_retry");
    }
  }
  listen("voice-model", (message) => showModel(message.payload));
  invoke("voice_model_status").then(showModel).catch(() => {});
})();
