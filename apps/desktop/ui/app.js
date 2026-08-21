/* ==========================================================================
   Bruma — interface.
   Nenhuma chave privada passa por aqui: o JavaScript pede ações, o Rust assina e cifra.
   ========================================================================== */

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (s, r = document) => r.querySelector(s);
const $$ = (s, r = document) => [...r.querySelectorAll(s)];

let vista = null;        // o último estado vindo do Rust
let servidorAtual = null;
let canalAtual = null;
let ligados = 0;

/* ---------- identicons: a chave pública desenhada ---------- */

function marcaDaChave(chave) {
  let h = 2166136261;
  for (const c of chave) { h ^= c.charCodeAt(0); h = Math.imul(h, 16777619); }
  let s = h >>> 0;
  const rnd = () => (s = (s * 1664525 + 1013904223) >>> 0) / 4294967296;
  const hue = 150 + Math.floor(rnd() * 130);
  const cor = `hsl(${hue} 42% 62%)`;
  const fundo = `hsl(${hue} 24% 16%)`;
  let celulas = '';
  const rect = (x, y) => `<rect x="${x}" y="${y}" width="1" height="1"/>`;
  for (let y = 0; y < 5; y++) {
    for (let x = 0; x < 3; x++) {
      if (rnd() > 0.48) { celulas += rect(x, y); if (x < 2) celulas += rect(4 - x, y); }
    }
  }
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="-0.4 -0.4 5.8 5.8">` +
    `<rect x="-0.4" y="-0.4" width="5.8" height="5.8" fill="${fundo}"/>` +
    `<g fill="${cor}">${celulas}</g></svg>`;
  return `url("data:image/svg+xml,${encodeURIComponent(svg)}")`;
}

function pintar(el, chave) {
  el.style.backgroundImage = marcaDaChave(chave || 'anon');
  el.style.backgroundSize = 'cover';
}

/* ---------- ajudas de UI ---------- */

const abrir = id => { $('#' + id).hidden = false; };
const fechar = id => { $('#' + id).hidden = true; };

function erroEm(id, msg) {
  const el = $('#' + id);
  el.textContent = msg || '';
}

/** Nunca usar innerHTML com texto de outra pessoa. */
function elemento(tag, cls, texto) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (texto !== undefined) e.textContent = texto;
  return e;
}

function chaveCurta(k) {
  return k ? `${k.slice(0, 4)}·${k.slice(4, 8)}·${k.slice(8, 12)}` : '';
}

/* ---------- desenhar ---------- */

function desenharRail() {
  const rail = $('#rail-servidores');
  rail.textContent = '';
  for (const s of vista.servidores) {
    const b = elemento('button', 'rail__pill', s.nome.slice(0, 2).toUpperCase());
    b.dataset.tip = s.nome;
    if (s.id === servidorAtual) b.classList.add('is-active');
    b.onclick = () => escolherServidor(s.id);
    rail.append(b);
  }
}

function servidor() {
  return vista.servidores.find(s => s.id === servidorAtual) || null;
}

function desenharCanais() {
  const lista = $('#lista-canais');
  lista.textContent = '';
  const s = servidor();
  if (!s) return;

  const grupos = [
    ['Texto', 'texto'],
    ['Voz', 'voz'],
  ];
  for (const [titulo, tipo] of grupos) {
    const canais = s.canais.filter(c => c.tipo === tipo);
    const g = elemento('div', 'group');
    const cab = elemento('div', 'group__label', titulo);
    const mais = elemento('button', 'porque', '+');
    mais.title = `Criar canal de ${titulo.toLowerCase()}`;
    mais.onclick = ev => { ev.stopPropagation(); abrirNovoCanal(tipo); };
    cab.append(mais);
    g.append(cab);

    for (const c of canais) {
      const b = elemento('button', 'chan');
      if (c.id === canalAtual) b.classList.add('is-active');
      const glifo = elemento('span', 'chan__glyph', tipo === 'voz' ? '♪' : '#');
      b.append(glifo, document.createTextNode(c.nome));
      const x = elemento('button', 'chan__x', '×');
      x.title = 'Apagar canal';
      x.onclick = async ev => {
        ev.stopPropagation();
        await invoke('apagar_canal', { servidor: s.id, canal: c.id }).catch(alertar);
      };
      b.append(x);
      b.onclick = () => escolherCanal(c.id);
      g.append(b);
    }
    lista.append(g);
  }
}

function desenharMembros() {
  const lista = $('#lista-membros');
  lista.textContent = '';
  const s = servidor();
  if (!s) return;
  $('#contagem-membros').textContent =
    s.membros.length === 1 ? '1 membro' : `${s.membros.length} membros`;
  for (const m of s.membros) {
    const linha = elemento('div', 'member');
    const av = elemento('span', 'ident');
    pintar(av, m.chave);
    const bloco = elemento('span');
    bloco.append(elemento('b', null, m.nome));
    bloco.append(elemento('i', null, m.fundador ? 'fundou este servidor' : chaveCurta(m.chave)));
    linha.append(av, bloco);
    lista.append(linha);
  }
}

async function desenharMensagens() {
  const stream = $('#stream');
  const s = servidor();
  const canal = s && s.canais.find(c => c.id === canalAtual);

  if (!s) {
    stream.textContent = '';
    const v = elemento('div', 'vazio');
    v.append(elemento('h3', null, 'Ainda não tens servidores'));
    v.append(elemento('p', null,
      'Cria um servidor teu ou entra num com um convite. Não é preciso registar nada em lado nenhum.'));
    const b = elemento('button', 'btn btn--primary', 'Começar');
    b.onclick = () => abrir('veu-novo');
    v.append(b);
    stream.append(v);
    $('#composer').hidden = true;
    return;
  }

  if (!canal) {
    stream.textContent = '';
    const v = elemento('div', 'vazio');
    v.append(elemento('h3', null, 'Escolhe um canal'));
    v.append(elemento('p', null, 'Ou cria um novo com o + ao lado de Texto.'));
    stream.append(v);
    $('#composer').hidden = true;
    return;
  }

  if (canal.tipo === 'voz') {
    stream.textContent = '';
    const v = elemento('div', 'vazio');
    v.append(elemento('h3', null, canal.nome));
    v.append(elemento('p', null,
      'Os canais de voz e a partilha de ecrã ainda não estão ligados nesta versão. ' +
      'A captura já foi validada, falta o transporte entre peers.'));
    stream.append(v);
    $('#composer').hidden = true;
    return;
  }

  $('#composer').hidden = false;
  $('#entrada').placeholder = `Mensagem para #${canal.nome}`;

  const msgs = await invoke('mensagens', { servidor: s.id, canal: canal.id }).catch(() => []);
  stream.textContent = '';
  if (!msgs.length) {
    const v = elemento('div', 'vazio');
    v.append(elemento('h3', null, `#${canal.nome}`));
    v.append(elemento('p', null, 'Ainda não há nada aqui. Escreve a primeira mensagem.'));
    stream.append(v);
    return;
  }

  let anterior = null;
  for (const m of msgs) {
    const seguida = anterior && anterior.autor === m.autor && m.ts_ms - anterior.ts_ms < 5 * 60_000;
    const art = elemento('article', seguida ? 'msg msg--cont' : 'msg');
    if (!seguida) {
      const av = elemento('span', 'ident ident--lg');
      pintar(av, m.autor);
      art.append(av);
    }
    const corpo = elemento('div', 'msg__body');
    if (!seguida) {
      const cab = elemento('div', 'msg__head');
      cab.append(elemento('b', null, m.autor_nome));
      const d = new Date(m.ts_ms);
      cab.append(elemento('time', null,
        `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`));
      corpo.append(cab);
    }
    corpo.append(elemento('p', null, m.texto));
    art.append(corpo);
    stream.append(art);
    anterior = m;
  }
  stream.scrollTop = stream.scrollHeight;
}

function desenharTopo() {
  const s = servidor();
  const canal = s && s.canais.find(c => c.id === canalAtual);
  $('#nome-servidor').textContent = s ? s.nome : '—';
  $('#nome-canal').textContent = canal ? canal.nome : '—';
  $('#glifo-canal').textContent = canal && canal.tipo === 'voz' ? '♪' : '#';
  $('#btn-convite').style.display = s ? '' : 'none';
  $('#rotulo-peers').textContent = ligados === 1 ? '1 ligado' : `${ligados} ligados`;
  $('#chip-peers').querySelector('.dot').className = ligados > 0 ? 'dot dot--ok' : 'dot';
}

async function desenharTudo() {
  vista = await invoke('estado');
  $('#meu-nome').textContent = vista.nome || 'sem nome';
  $('#minha-chave').textContent = chaveCurta(vista.chave);
  pintar($('#meu-avatar'), vista.chave);

  if (!vista.servidores.some(s => s.id === servidorAtual)) {
    servidorAtual = vista.servidores[0] ? vista.servidores[0].id : null;
    canalAtual = null;
  }
  const s = servidor();
  if (s && !s.canais.some(c => c.id === canalAtual)) {
    const primeiro = s.canais.find(c => c.tipo === 'texto') || s.canais[0];
    canalAtual = primeiro ? primeiro.id : null;
  }

  desenharRail();
  desenharCanais();
  desenharMembros();
  desenharTopo();
  await desenharMensagens();
}

function escolherServidor(id) {
  servidorAtual = id;
  canalAtual = null;
  desenharTudo();
}

function escolherCanal(id) {
  canalAtual = id;
  desenharCanais();
  desenharTopo();
  desenharMensagens();
}

function alertar(e) {
  console.error(e);
  erroEm('erro-novo', String(e));
}

/* ---------- ações ---------- */

$('#btn-novo').onclick = () => { erroEm('erro-novo', ''); abrir('veu-novo'); };
$('#fechar-novo').onclick = () => fechar('veu-novo');

$('#ok-servidor').onclick = async () => {
  const nome = $('#in-servidor').value.trim();
  if (!nome) return erroEm('erro-novo', 'dá um nome ao servidor');
  try {
    const id = await invoke('criar_servidor', { nome });
    $('#in-servidor').value = '';
    fechar('veu-novo');
    servidorAtual = id; canalAtual = null;
    await desenharTudo();
  } catch (e) { erroEm('erro-novo', String(e)); }
};

$('#ok-convite').onclick = async () => {
  const codigo = $('#in-convite').value.trim();
  if (!codigo) return erroEm('erro-novo', 'cola o código do convite');
  erroEm('erro-novo', 'a ligar ao anfitrião…');
  try {
    const id = await invoke('entrar_com_convite', { codigo });
    $('#in-convite').value = '';
    fechar('veu-novo');
    servidorAtual = id; canalAtual = null;
    await desenharTudo();
  } catch (e) { erroEm('erro-novo', String(e)); }
};

function abrirNovoCanal(tipo) {
  $('#in-canal').value = '';
  $('#in-tipo').value = tipo || 'texto';
  erroEm('erro-canal', '');
  abrir('veu-canal');
  $('#in-canal').focus();
}
$('#fechar-canal').onclick = () => fechar('veu-canal');
$('#ok-canal').onclick = async () => {
  const nome = $('#in-canal').value.trim();
  if (!nome) return erroEm('erro-canal', 'dá um nome ao canal');
  try {
    await invoke('criar_canal', { servidor: servidorAtual, nome, tipo: $('#in-tipo').value });
    fechar('veu-canal');
    await desenharTudo();
  } catch (e) { erroEm('erro-canal', String(e)); }
};

$('#btn-convite').onclick = async () => {
  try {
    const codigo = await invoke('criar_convite', { servidor: servidorAtual });
    $('#out-convite').value = codigo;
    $('#copiado').textContent = '';
    abrir('veu-convite');
  } catch (e) { console.error(e); }
};
$('#fechar-convite').onclick = () => fechar('veu-convite');
$('#copiar-convite').onclick = async () => {
  await navigator.clipboard.writeText($('#out-convite').value);
  $('#copiado').textContent = 'copiado';
};

$('#btn-perfil').onclick = () => {
  $('#in-nome').value = vista.nome || '';
  erroEm('erro-nome', '');
  abrir('veu-bemvindo');
  $('#in-nome').focus();
};
$('#ok-nome').onclick = async () => {
  const nome = $('#in-nome').value.trim();
  if (!nome) return erroEm('erro-nome', 'escreve um nome');
  try {
    await invoke('definir_nome', { nome });
    fechar('veu-bemvindo');
    await desenharTudo();
  } catch (e) { erroEm('erro-nome', String(e)); }
};

$('#entrada').addEventListener('keydown', async ev => {
  if (ev.key !== 'Enter' || !ev.target.value.trim()) return;
  const texto = ev.target.value;
  ev.target.value = '';
  try {
    await invoke('enviar', { servidor: servidorAtual, canal: canalAtual, texto });
    await desenharMensagens();
  } catch (e) { console.error(e); }
});

/* ---------- eventos vindos do núcleo ---------- */

listen('servidor-mudou', async ev => {
  await desenharTudo();
});
listen('peer-ligado', () => { ligados += 1; desenharTopo(); });
listen('peer-desligado', () => { ligados = Math.max(0, ligados - 1); desenharTopo(); });

/* ---------- explicações: o porquê vive na app ---------- */

const EXPLICACOES = {
  identidade: {
    titulo: 'A tua identidade',
    corpo: [
      'Foi criada neste computador na primeira vez que abriste a app. É uma chave, e é ao mesmo tempo o teu ID e o teu endereço na rede.',
      'Não existe conta, não existe registo, e ninguém — nem tu — a pode recuperar se apagares a pasta de dados.',
    ],
  },
  e2ee: {
    titulo: 'Cifrado ponta a ponta',
    corpo: [
      'As mensagens são cifradas <b>antes</b> de saírem deste computador, com uma chave que só os membros do servidor têm.',
      'O que <b>não</b> esconde: quem fala com quem e quando. Isso chama-se metadados.',
    ],
  },
  caminho: {
    titulo: 'Quem está ligado',
    corpo: [
      'Não há servidor. Isto conta quantos membros estão ligados a ti <b>neste momento</b>, diretamente.',
      'É com eles que o teu histórico sincroniza. Se não houver ninguém ligado, nada de novo chega — e nada do que escreveres sai daqui até alguém aparecer.',
    ],
  },
  historico: {
    titulo: 'Porque é que quem está online importa',
    corpo: [
      'O histórico deste servidor existe nos computadores dos membros, e mais em lado nenhum.',
      '<b>Se ninguém do servidor estiver online, não há nada de onde puxar.</b> É o preço direto de não haver uma máquina no meio.',
    ],
  },
  expulsar: {
    titulo: 'Membros e chaves',
    corpo: [
      'Quem aparece aqui é quem já escreveu alguma coisa neste servidor. A identidade vem da assinatura de cada entrada, não de um registo.',
      'O <b>convite contém a chave do servidor</b>: quem o tiver consegue ler tudo o que for escrito a partir do momento em que entra. Trata-o como um segredo.',
    ],
  },
};

const painelExplica = $('#explica');
function mostrarExplicacao(chave, ancora) {
  const e = EXPLICACOES[chave];
  if (!e) return;
  $('#explica-titulo').textContent = e.titulo;
  const corpo = $('#explica-corpo');
  corpo.textContent = '';
  for (const p of e.corpo) {
    const el = document.createElement('p');
    el.innerHTML = p;   // literais desta constante, nunca dados de fora
    corpo.append(el);
  }
  painelExplica.hidden = false;
  const r = ancora.getBoundingClientRect();
  const largura = painelExplica.offsetWidth;
  let x = Math.max(12, Math.min(r.left + r.width / 2 - largura / 2, innerWidth - largura - 12));
  let y = r.bottom + 8;
  if (y + painelExplica.offsetHeight > innerHeight - 12) {
    y = Math.max(12, r.top - painelExplica.offsetHeight - 8);
  }
  painelExplica.style.left = `${Math.round(x)}px`;
  painelExplica.style.top = `${Math.round(y)}px`;
}
const esconderExplicacao = () => { painelExplica.hidden = true; };

document.addEventListener('click', ev => {
  const gatilho = ev.target.closest('[data-explica]');
  if (gatilho) {
    ev.stopPropagation();
    if (!painelExplica.hidden && painelExplica.dataset.chave === gatilho.dataset.explica) {
      return esconderExplicacao();
    }
    painelExplica.dataset.chave = gatilho.dataset.explica;
    return mostrarExplicacao(gatilho.dataset.explica, gatilho);
  }
  if (!ev.target.closest('#explica')) esconderExplicacao();
});
document.addEventListener('keydown', ev => {
  if (ev.key === 'Escape') esconderExplicacao();
});

/* A névoa é um blur de ecrã inteiro: não gastar GPU com a janela escondida. */
document.addEventListener('visibilitychange', () => {
  const fog = $('.fog');
  if (fog) fog.style.animationPlayState = document.hidden ? 'paused' : 'running';
});

/* ---------- arranque ---------- */

(async () => {
  await desenharTudo();
  if (!vista.nome) {
    abrir('veu-bemvindo');
    $('#in-nome').focus();
  }
})();
