/* ==========================================================================
   Bruma — shell + diagnóstico do spike 2
   ========================================================================== */

const $ = (s, r = document) => r.querySelector(s);
const $$ = (s, r = document) => [...r.querySelectorAll(s)];
const sleep = ms => new Promise(r => setTimeout(r, ms));

/* --------------------------------------------------------------------------
   Identicons — a chave pública desenhada.
   Não há fotografias de perfil numa app onde ninguém dá dados. O avatar é
   derivado da chave, portanto duas pessoas nunca têm a mesma marca e não é
   preciso confiar em ninguém para a gerar.
   -------------------------------------------------------------------------- */

function marcaDaChave(chave) {
  // FNV-1a: determinística e chega perfeitamente para isto.
  let h = 2166136261;
  for (const c of chave) { h ^= c.charCodeAt(0); h = Math.imul(h, 16777619); }
  let s = h >>> 0;
  const rnd = () => (s = (s * 1664525 + 1013904223) >>> 0) / 4294967296;

  // Matiz limitada ao azul-violeta-verde frio, para não sair da paleta da app.
  const hue = 150 + Math.floor(rnd() * 130);
  const cor = `hsl(${hue} 42% 62%)`;
  const fundo = `hsl(${hue} 24% 16%)`;

  // 5x5 espelhada na vertical: só 3 colunas são realmente aleatórias.
  let celulas = '';
  const rect = (x, y) => `<rect x="${x}" y="${y}" width="1" height="1"/>`;
  for (let y = 0; y < 5; y++) {
    for (let x = 0; x < 3; x++) {
      if (rnd() > 0.48) {
        celulas += rect(x, y);
        if (x < 2) celulas += rect(4 - x, y);
      }
    }
  }
  const svg =
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="-0.4 -0.4 5.8 5.8">` +
    `<rect x="-0.4" y="-0.4" width="5.8" height="5.8" fill="${fundo}"/>` +
    `<g fill="${cor}">${celulas}</g></svg>`;
  return `url("data:image/svg+xml,${encodeURIComponent(svg)}")`;
}

$$('.ident').forEach(el => {
  el.style.backgroundImage = marcaDaChave(el.dataset.key || 'anon');
  el.style.backgroundSize = 'cover';
});

/* A névoa é um blur de ecrã inteiro a animar em ciclo. Não há razão para gastar
   GPU enquanto a janela nem sequer está a ser vista. */
document.addEventListener('visibilitychange', () => {
  const fog = $('.fog');
  if (fog) fog.style.animationPlayState = document.hidden ? 'paused' : 'running';
});

/* --------------------------------------------------------------------------
   Navegação entre vistas
   -------------------------------------------------------------------------- */

const VISTAS = {
  geral:  { nome: 'geral',       diagnostico: false },
  spikes: { nome: 'spikes',      diagnostico: false },
  diag:   { nome: 'diagnóstico', diagnostico: true  },
  voz:    { nome: 'Sala da névoa', diagnostico: false },
};

$$('.chan[data-view]').forEach(btn => {
  btn.onclick = () => {
    $$('.chan').forEach(b => b.classList.remove('is-active'));
    btn.classList.add('is-active');
    const v = VISTAS[btn.dataset.view];
    $('#view-name').textContent = v.nome;
    $('#view-chat').hidden = v.diagnostico;
    $('#view-diag').hidden = !v.diagnostico;
    $('#composer').style.display = v.diagnostico ? 'none' : '';
    $('#input').placeholder = `Mensagem para #${v.nome}`;
  };
});

/* --------------------------------------------------------------------------
   Modo Fantasma
   O botão tem de dizer o que faz E o que deixa de funcionar. Prometer
   "anónimo" sem mencionar que a voz morre seria mentir por omissão.
   -------------------------------------------------------------------------- */

const NOTA_NORMAL = 'as mensagens ficam no teu computador — não há servidor onde elas se acumulem';
const NOTA_FANTASMA =
  'Modo Fantasma ligado · ninguém fica a saber o teu IP, nem os outros membros nem o relay. ' +
  'Em troca: o chat fica mais lento, e voz, vídeo e partilha de ecrã ficam indisponíveis — ' +
  'o Tor só transporta TCP, e o WebRTC precisa de UDP.';

$('#ghost-toggle').onclick = e => {
  const ligado = document.body.classList.toggle('ghost');
  e.currentTarget.classList.toggle('is-on', ligado);
  $('#path-label').textContent = ligado ? 'fantasma' : 'direto';
  $('#chip-path').classList.toggle('chip--warn', ligado);
  $('.dot--ok', $('#chip-path'))?.classList.toggle('dot--warn', ligado);
  $('#composer-note').textContent = ligado ? NOTA_FANTASMA : NOTA_NORMAL;
};

/* --------------------------------------------------------------------------
   Conversa de mentira, só para a vista ter vida
   -------------------------------------------------------------------------- */

$('#input').addEventListener('keydown', ev => {
  if (ev.key !== 'Enter' || !ev.target.value.trim()) return;
  const texto = ev.target.value.trim();
  ev.target.value = '';
  const art = document.createElement('article');
  art.className = 'msg';
  const agora = new Date();
  const hh = String(agora.getHours()).padStart(2, '0');
  const mm = String(agora.getMinutes()).padStart(2, '0');
  art.innerHTML =
    `<span class="ident ident--lg" data-key="27a76005040e"></span>
     <div class="msg__body">
       <div class="msg__head"><b>tu</b><time>${hh}:${mm}</time></div>
       <p></p>
     </div>`;
  art.querySelector('p').textContent = texto;   // textContent: nunca injetar HTML do utilizador
  const marca = art.querySelector('.ident');
  marca.style.backgroundImage = marcaDaChave('27a76005040e');
  marca.style.backgroundSize = 'cover';
  $('.typing').before(art);
  $('#view-chat').scrollTop = $('#view-chat').scrollHeight;
});

/* ==========================================================================
   Diagnóstico do spike 2 — a parte que não é decorativa
   ========================================================================== */

const relatorio = {
  ambiente: {
    userAgent: navigator.userAgent,
    nucleos: navigator.hardwareConcurrency,
  },
  captura: null,
  codecs: null,
  medicoes: [],
};
let stream = null;

const desenharRelatorio = () => { $('#report').textContent = JSON.stringify(relatorio, null, 2); };
desenharRelatorio();

/* --- escrita segura no DOM ---
   Isto nao e zelo excessivo. O v.label e o TITULO da janela que se esta a partilhar, e
   qualquer pagina web o controla via document.title. Com innerHTML, bastava um amigo mandar
   um link, abrires a pagina e partilhares essa janela para correr script dentro da app -- e
   numa app Tauri isso alcanca a ponte IPC. O JSON.stringify nao salva: escapa aspas, mas
   deixa passar < e >. */

function escaparHtml(v) {
  return String(v ?? '').replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
}

/** Escreve o painel de captura sem passar por innerHTML. */
function mostrarCaptura(linhas, dados) {
  const out = $('#cap-out');
  out.textContent = '';
  for (const l of linhas) {
    if (l.classe) {
      const span = document.createElement('span');
      span.className = l.classe;
      span.textContent = l.texto;
      out.append(span);
    } else {
      out.append(l.texto);
    }
    if (l.extra) out.append(l.extra);
    out.append('\n');
  }
  out.append('\n' + JSON.stringify(dados, null, 2));
}

/* --- 1. captura --- */

async function capturar(comAudio) {
  const pedido = {
    video: { frameRate: { ideal: 60 } },
    audio: comAudio ? { echoCancellation: false, noiseSuppression: false } : false,
    surfaceSwitching: 'include',
    selfBrowserSurface: 'exclude',
  };
  if (comAudio) pedido.systemAudio = 'include';

  const t0 = performance.now();
  try {
    stream = await navigator.mediaDevices.getDisplayMedia(pedido);
  } catch (e) {
    const r = {
      ok: false, pedidoComAudio: comAudio,
      erro: `${e.name}: ${e.message}`,
      msAteFalhar: Math.round(performance.now() - t0),
    };
    relatorio.captura = r;
    mostrarCaptura([{ classe: 'bad', texto: 'FALHOU' }], r);
    desenharRelatorio();
    return;
  }

  const msAteResolver = Math.round(performance.now() - t0);
  const v = stream.getVideoTracks()[0];
  const a = stream.getAudioTracks()[0];
  const s = v ? v.getSettings() : {};

  const r = {
    ok: true,
    pedidoComAudio: comAudio,
    msAteResolver,
    // Acima de um segundo houve interação humana, logo apareceu uma janela de escolha.
    houvePickerProvavelmente: msAteResolver > 1000,
    video: {
      label: v?.label ?? null,
      resolucao: s.width && s.height ? `${s.width}x${s.height}` : null,
      frameRate: s.frameRate ?? null,
      displaySurface: s.displaySurface ?? '(nao reportado)',
      cursor: s.cursor ?? '(nao reportado)',
    },
    audioObtido: !!a,
    audio: a ? { label: a.label, settings: a.getSettings() } : null,
  };
  relatorio.captura = r;

  mostrarCaptura([
    { classe: 'ok', texto: 'CAPTURA OK' },
    r.houvePickerProvavelmente
      ? { classe: 'ok', texto: 'picker de fontes: apareceu',
          extra: `  (${msAteResolver} ms ate resolver)` }
      : { classe: 'warn', texto: 'picker de fontes: NAO parece ter aparecido',
          extra: `  (resolveu em ${msAteResolver} ms)` },
    comAudio
      ? (r.audioObtido
          ? { classe: 'ok', texto: 'audio do sistema: obtido' }
          : { classe: 'bad', texto: 'audio do sistema: nenhuma track veio' })
      : { texto: 'audio: nao pedido' },
  ], r);

  $('#preview').srcObject = stream;
  $('#btn-stop').disabled = false;
  $('#btn-bitrate').disabled = false;
  $('#btn-bitrate-quick').disabled = false;
  v.addEventListener('ended', pararCaptura);
  desenharRelatorio();
}

function pararCaptura() {
  stream?.getTracks().forEach(t => t.stop());
  stream = null;
  $('#preview').srcObject = null;
  $('#btn-stop').disabled = true;
  $('#btn-bitrate').disabled = true;
  $('#btn-bitrate-quick').disabled = true;
}

$('#btn-cap').onclick = () => capturar(false);
$('#btn-cap-audio').onclick = () => capturar(true);
$('#btn-stop').onclick = pararCaptura;

/* --- 2. codecs --- */

$('#btn-codecs').onclick = () => {
  const caps = RTCRtpSender.getCapabilities('video');
  const lista = caps.codecs.map(c => ({ mime: c.mimeType, clock: c.clockRate, params: c.sdpFmtpLine || null }));
  relatorio.codecs = lista;
  const familias = [...new Set(lista.map(c => c.mime.split('/')[1]))];
  $('#codec-out').textContent = `familias: ${familias.join(', ')}\n\n` + JSON.stringify(lista, null, 2);
  desenharRelatorio();
};

/* --- 3. bitrate por loopback local --- */

async function amostra(pc) {
  const stats = await pc.getStats();
  let out = null, codecId = null;
  stats.forEach(r => {
    if (r.type === 'outbound-rtp' && r.kind === 'video') {
      out = {
        ts: r.timestamp, bytes: r.bytesSent, frames: r.framesEncoded,
        encoder: r.encoderImplementation ?? '(nao reportado)',
        limite: r.qualityLimitationReason ?? null,
        w: r.frameWidth, h: r.frameHeight,
      };
      codecId = r.codecId;
    }
  });
  if (out && codecId) stats.forEach(r => { if (r.id === codecId) out.codec = r.mimeType; });
  return out;
}

async function medir(mime, hint, segundos) {
  const track = stream.getVideoTracks()[0];
  track.contentHint = hint;

  const pc1 = new RTCPeerConnection();
  const pc2 = new RTCPeerConnection();
  try {
  pc1.onicecandidate = e => e.candidate && pc2.addIceCandidate(e.candidate);
  pc2.onicecandidate = e => e.candidate && pc1.addIceCandidate(e.candidate);
  pc2.ontrack = () => {};

  const tx = pc1.addTransceiver(track, { direction: 'sendonly' });
  const todos = RTCRtpSender.getCapabilities('video').codecs;
  const alvo = todos.filter(c => c.mimeType.toLowerCase() === mime.toLowerCase());
  if (alvo.length) tx.setCodecPreferences([...alvo, ...todos.filter(c => !alvo.includes(c))]);

  await pc1.setLocalDescription(await pc1.createOffer());
  await pc2.setRemoteDescription(pc1.localDescription);
  await pc2.setLocalDescription(await pc2.createAnswer());
  await pc1.setRemoteDescription(pc2.localDescription);

  await sleep(3500);                      // deixar o encoder estabilizar
  const a1 = await amostra(pc1);
  await sleep(segundos * 1000);
  const a2 = await amostra(pc1);

  if (!a1 || !a2) return { mime, hint: hint || '(nenhum)', erro: 'sem estatisticas' };
  const dt = (a2.ts - a1.ts) / 1000;
  return {
    mime,
    hint: hint || '(nenhum)',
    codecUsado: a2.codec ?? '?',
    codecPedidoDisponivel: alvo.length > 0,
    kbps: Math.round((a2.bytes - a1.bytes) * 8 / dt / 1000),
    fps: +(((a2.frames - a1.frames) / dt).toFixed(1)),
    resolucao: a2.w && a2.h ? `${a2.w}x${a2.h}` : '?',
    encoder: a2.encoder,
    limitadoPor: a2.limite,
  };
  } finally {
    // Fechar aqui e nao no fim do caminho feliz: se a negociacao SDP falhar a meio, as duas
    // ligacoes ficavam abertas e a track de ecra presa. Ao fim das oito medicoes do teste
    // completo seriam ate dezasseis ligacoes penduradas.
    pc1.close();
    pc2.close();
  }
}

function desenharTabela(linhas) {
  const cols = ['pedido', 'hint', 'usado', 'kbps', 'fps', 'resolucao', 'encoder', 'limitado'];
  const th = cols.map(c => `<th>${c}</th>`).join('');
  const tr = linhas.map(r => {
    if (r.erro) return `<tr><td>${escaparHtml(r.mime)}</td><td>${escaparHtml(r.hint)}</td><td colspan="6" class="bad">${escaparHtml(r.erro)}</td></tr>`;
    const hw = r.encoder && !/libaom|libvpx|openh264|software|ffmpeg/i.test(r.encoder);
    return `<tr>
      <td>${escaparHtml(r.mime.replace('video/', ''))}</td><td>${escaparHtml(r.hint)}</td><td>${escaparHtml((r.codecUsado || '').replace('video/', ''))}</td>
      <td class="num">${escaparHtml(r.kbps)}</td><td class="num">${escaparHtml(r.fps)}</td><td>${escaparHtml(r.resolucao)}</td>
      <td class="${hw ? 'ok' : 'warn'}">${escaparHtml(r.encoder)}</td><td>${escaparHtml(r.limitadoPor ?? '-')}</td></tr>`;
  }).join('');
  $('#table-wrap').innerHTML = `<table><thead><tr>${th}</tr></thead><tbody>${tr}</tbody></table>`;
}

async function correrMedicoes(combos, segundos) {
  $('#btn-bitrate').disabled = true;
  $('#btn-bitrate-quick').disabled = true;
  const cenario = $('#scenario').value;
  const linhas = [];
  try {
  for (let i = 0; i < combos.length; i++) {
    const [mime, hint] = combos[i];
    $('#progress').textContent = `a medir ${i + 1}/${combos.length} · ${mime.replace('video/', '')} · hint "${hint || 'nenhum'}"`;
    const r = await medir(mime, hint, segundos);
    r.cenario = cenario;
    linhas.push(r);
    desenharTabela(linhas);
    relatorio.medicoes = relatorio.medicoes.filter(m => m.cenario !== cenario).concat(linhas);
    desenharRelatorio();
  }
  $('#progress').textContent = `terminado · ${combos.length} medicoes no cenario "${cenario}"`;
  } catch (e) {
    $('#progress').textContent = `interrompido por erro: ${e.message}`;
    console.error(e);
  } finally {
    // Sem isto, uma excecao a meio deixava os dois botoes desativados para sempre: a unica
    // saida seria recarregar, e o relatorio ja recolhido perdia-se.
    $('#btn-bitrate').disabled = false;
    $('#btn-bitrate-quick').disabled = false;
  }
}

$('#btn-bitrate-quick').onclick = () => correrMedicoes([['video/AV1', 'text'], ['video/AV1', '']], 8);
$('#btn-bitrate').onclick = () => correrMedicoes([
  ['video/AV1', 'text'], ['video/AV1', 'detail'], ['video/AV1', 'motion'], ['video/AV1', ''],
  ['video/VP9', 'text'], ['video/VP9', ''],
  ['video/VP8', 'text'], ['video/H264', 'text'],
], 8);

/* --- 4. relatório --- */

$('#btn-copy').onclick = async e => {
  await navigator.clipboard.writeText(JSON.stringify(relatorio, null, 2));
  const b = e.currentTarget;
  b.textContent = 'copiado';
  setTimeout(() => (b.textContent = 'Copiar relatório'), 1500);
};
