// Sesli sohbet: a conversation by voice, as on the phones. What the person says is recognised on this computer (the same on-device model as dictation),
// pieces are joined into one turn, and the turn is sent only after a pause that fits how the words end (the phones' voice-turn rules: short after a
// question or a full stop, long after "ve", "ama", "çünkü"…). The answer is read aloud; speaking over it stops the reading.
(() => {
  const { invoke } = window.__TAURI__.core;
  const $ = (id) => document.getElementById(id);
  const button = $("talk-button");
  const input = $("chat-input");
  if (!button) return;

  // ----- when the person has finished their turn (RovaAndroidRN/src/data/voice-turn.ts) -----
  const STILL_TALKING = /(?:^|\s)(?:ve|ama|fakat|ancak|çünkü|yani|şey|ile|veya|ya da|mesela|hani|sonra|da|de|ki|eee+|ııı+|hmm+|and|but|because|so|or|um+|uh+)\s*$/iu;
  function turnPauseMs(heard) {
    const text = heard.trim();
    if (!text) return 1200;
    if (STILL_TALKING.test(text)) return 1900;
    if (/[?!.…]$/u.test(text)) return 650;
    return text.split(/\s+/).length <= 2 ? 1300 : 1000;
  }
  function mergeHeard(turn, piece) {
    const before = turn.trim(), next = piece.trim();
    if (!before) return next;
    if (!next) return before;
    const fold = (value) => value.toLocaleLowerCase("tr").replace(/[^\p{L}\p{N}\s]/gu, "").replace(/\s+/g, " ").trim();
    if (fold(next).startsWith(fold(before))) return next;
    if (fold(before).endsWith(fold(next))) return before;
    return `${before} ${next}`;
  }

  // ----- reading the answer aloud -----
  function spoken(markdown) {
    return markdown
      .replace(/```[\s\S]*?```/g, " Kod bloğu ekranda. ")
      .replace(/!\[[^\]]*\]\([^)]*\)/g, "")
      .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
      .replace(/^\s*\|.*\|\s*$/gm, "")
      .replace(/^#{1,6}\s*/gm, "")
      .replace(/[*_`>~]/g, "")
      .replace(/https?:\/\/\S+/g, "")
      .replace(/\n{2,}/g, ". ")
      .replace(/\s+/g, " ")
      .trim()
      .slice(0, 1400);
  }
  let voice = null;
  const pickVoice = () => {
    const voices = speechSynthesis.getVoices();
    voice = voices.find((item) => /^tr/i.test(item.lang) && /premium|enhanced|siri/i.test(item.name)) || voices.find((item) => /^tr/i.test(item.lang)) || null;
  };
  if ("speechSynthesis" in window) { pickVoice(); speechSynthesis.onvoiceschanged = pickVoice; }

  function speak(text) {
    const words = spoken(text);
    if (!words || !("speechSynthesis" in window)) { listenAgain(); return; }
    speechSynthesis.cancel();
    const utterance = new SpeechSynthesisUtterance(words);
    utterance.lang = voice?.lang || "tr-TR";
    if (voice) utterance.voice = voice;
    utterance.rate = 1.04;
    utterance.onend = utterance.onerror = () => { if (state === "speaking") listenAgain(); };
    setState("speaking");
    speechSynthesis.speak(utterance);
  }

  // ----- the conversation -----
  const START = 0.018;
  const PHRASE_PAUSE = 700;
  let session = null;
  let state = "off";
  let heard = "";
  let turnTimer = null;
  let queue = Promise.resolve();
  let pending = 0;
  const placeholder = input.placeholder;
  const LABEL = { listening: "Dinliyorum…", hearing: "Dinliyorum…", waiting: "Wiqo düşünüyor…", speaking: "Wiqo konuşuyor; araya girebilirsin." };

  function setState(next) {
    state = next;
    button.classList.toggle("on", next !== "off");
    button.classList.toggle("talking", next === "speaking");
    input.placeholder = next === "off" ? placeholder : heard ? `“${heard}”` : LABEL[next] || placeholder;
  }
  function listenAgain() { if (session) setState("listening"); }

  function scheduleTurn() {
    clearTimeout(turnTimer);
    if (!heard || pending) return;
    turnTimer = setTimeout(() => {
      if (!session || pending || session.speechAt || !heard) return;
      if (window.WiqoChat?.busy()) { scheduleTurn(); return; }
      const text = heard;
      heard = "";
      setState("waiting");
      window.WiqoChat?.send(text, { spoken: true });
    }, turnPauseMs(heard));
  }

  function recognise(frames, rate) {
    pending += 1;
    queue = queue.then(async () => {
      try {
        const { toWav, base64Of } = window.WiqoVoice;
        const words = ((await invoke("listen_transcribe", { audio: base64Of(toWav(frames, rate)), language: (navigator.language || "tr").slice(0, 2).toLowerCase() })) || "").trim();
        if (words && session) { heard = mergeHeard(heard, words); setState(state === "waiting" ? "waiting" : "listening"); }
      } catch (error) {
        window.toast?.(String(error), "bad");
      } finally {
        pending -= 1;
        scheduleTurn();
      }
    });
  }

  async function begin() {
    if (window.WiqoVoice?.busy()) return window.toast?.("Önce dikteyi bitir.", "bad");
    const status = await invoke("voice_model_status").catch(() => null);
    if (status && status.state !== "ready") {
      window.toast?.(status.state === "downloading" ? `Ses paketi iniyor (%${Math.round(status.progress * 100)}); bitince konuşabilirsin.` : (status.message || "Ses paketi henüz yok."), "bad");
      if (status.state === "failed" || status.state === "missing") invoke("voice_model_retry");
      return;
    }
    let stream;
    try {
      stream = await navigator.mediaDevices.getUserMedia({ audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true } });
    } catch (error) {
      return window.toast?.(`Mikrofon açılamadı (${error?.name || "izin"}). Sistem ayarlarından Wiqo'ya mikrofon izni ver.`, "bad");
    }
    window.showSection?.("chat");
    const context = new AudioContext();
    const source = context.createMediaStreamSource(stream);
    const processor = context.createScriptProcessor(2048, 1, 1);
    const mute = context.createGain();
    mute.gain.value = 0;
    source.connect(processor);
    processor.connect(mute).connect(context.destination);
    session = { stream, context, source, processor, preRoll: [], frames: [], speechAt: 0, quietAt: 0, loudSince: 0 };
    heard = "";
    setState("listening");
    processor.onaudioprocess = (event) => {
      if (!session) return;
      const frame = new Float32Array(event.inputBuffer.getChannelData(0));
      const level = Math.sqrt(frame.reduce((sum, value) => sum + value * value, 0) / frame.length);
      button.style.setProperty("--level", String(Math.min(1, level * 9)));
      const now = Date.now();
      if (state === "speaking") {
        // Speaking over the answer stops it (a little louder and longer than a sound, so the answer's own echo does not).
        if (level > START * 1.8) { session.loudSince ||= now; if (now - session.loudSince > 250) { speechSynthesis.cancel(); setState("listening"); } }
        else session.loudSince = 0;
        if (state === "speaking") return;
      }
      if (!session.speechAt) {
        session.preRoll.push(frame);
        if (session.preRoll.length > 8) session.preRoll.shift();
        if (level > START) { session.speechAt = now; clearTimeout(turnTimer); session.frames.push(...session.preRoll); session.preRoll = []; }
        return;
      }
      session.frames.push(frame);
      if (level > START * 0.7) session.quietAt = 0;
      else {
        session.quietAt ||= now;
        if (now - session.quietAt > PHRASE_PAUSE || now - session.speechAt > 25_000) {
          const long = now - session.speechAt > 450;
          const frames = session.frames;
          session.frames = [];
          session.speechAt = 0;
          session.quietAt = 0;
          if (long) recognise(frames, session.context.sampleRate);
          else scheduleTurn();
        }
      }
    };
  }

  function end() {
    const done = session;
    session = null;
    clearTimeout(turnTimer);
    if ("speechSynthesis" in window) speechSynthesis.cancel();
    heard = "";
    setState("off");
    button.style.removeProperty("--level");
    if (!done) return;
    done.processor.onaudioprocess = null;
    done.processor.disconnect();
    done.source.disconnect();
    done.stream.getTracks().forEach((track) => track.stop());
    setTimeout(() => done.context.close(), 100);
  }

  window.addEventListener("wiqo-reply", (event) => {
    if (!session || state !== "waiting") return;
    if (event.detail.error) { listenAgain(); return; }
    speak(event.detail.text);
  });
  button.onclick = () => (session ? end() : begin());
  document.addEventListener("keydown", (event) => { if (event.key === "Escape" && session) end(); });
})();
