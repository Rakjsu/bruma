/* ==========================================================================
   Bruma — interface.
   Nenhuma chave privada passa por aqui: o JavaScript pede ações, o Rust assina e cifra.
   ========================================================================== */

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

/* ==========================================================================
   Um erro de JavaScript deixa de desaparecer.

   Tudo o que corri hoje para tornar falhas visíveis parou na fronteira do JS: uma excepção
   não apanhada ia para um DevTools que ninguém abre, e o sintoma era a interface deixar de
   responder a meio sem uma palavra em lado nenhum. Foi assim que uma medição minha parou a
   meio e não deu pista nenhuma.

   Agora vai para o mesmo `bruma.log` de tudo o resto.
   ========================================================================== */
(function erros() {
  const contar = (o_que, msg, onde) => {
    try {
      window.__TAURI__.core.invoke('capacidades', {
        linha: `[js] ${o_que}: ${msg}${onde ? ' @ ' + onde : ''}`,
      }).catch(() => {});
    } catch (e) { /* nem o registo há; não há mais nada a fazer */ }
  };
  window.addEventListener('error', ev => {
    contar('erro', ev.message, `${ev.filename || '?'}:${ev.lineno}:${ev.colno}`);
  });
  window.addEventListener('unhandledrejection', ev => {
    const r = ev.reason;
    contar('promessa recusada', (r && (r.stack || r.message)) || String(r), '');
  });
})();

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
      b.dataset.canal = c.id;
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
      if (tipo === 'voz') {
        const dentro = [...voz.presentes.entries()].filter(([, k]) => k === c.id).map(([p]) => p);
        if (voz.canal === c.id) dentro.unshift(voz.eu);
        if (dentro.length) {
          const lista = elemento('div', 'voice-members');
          for (const p of dentro) {
            const linha = elemento('div', 'vm');
            const av = elemento('span', 'ident');
            pintar(av, p);
            linha.append(av, document.createTextNode(p === voz.eu ? 'tu' : nomeDoPeer(p)));
            lista.append(linha);
          }
          g.append(lista);
        }
      }
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
    linha.dataset.chave = m.chave;
    if (voz.falando.has(m.chave)) linha.classList.add('a-falar');
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
    stream.hidden = true;
    $('#composer').hidden = true;
    desenharVoz();
    return;
  }
  stream.hidden = false;
  $('#vista-voz').hidden = true;
  desenharNaChamada();

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
  desenharRodape();
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

$('#btn-perfil').onclick = () => abrirDefinicoes();

/* ==========================================================================
   Definições, em ecrã inteiro.

   # A regra que manda aqui

   Cinco das secções não têm nada por trás — e é isso que elas dizem. A tentação
   era enchê-las com interruptores plausíveis, e seria a pior coisa possível: um
   interruptor que não faz nada é uma mentira que a pessoa só descobre quando
   precisava dele. Ficam listadas, a cinzento, a explicar o que falta e porquê.

   Uma delas nunca vai ter nada, e isso é uma característica: não há cobranças
   porque não há servidor para pagar nem subscrição para vender.
   ========================================================================== */

const SEM_MOVIMENTO = 'bruma.sem-movimento';
const SEM_JOGO = 'bruma.sem-deteccao-de-jogo';

/** Se a deteção de jogo está desligada. Lê-se em `desenharJogo`. */
function deteccaoDeJogoDesligada() {
  return localStorage.getItem(SEM_JOGO) === '1';
}

function aplicarMovimento() {
  document.documentElement.classList.toggle(
    'sem-movimento', localStorage.getItem(SEM_MOVIMENTO) === '1');
}
aplicarMovimento();

/** Um interruptor com título e explicação, como os do Discord. */
function interruptor(titulo, explica, ligado, aoMudar) {
  const l = elemento('label', 'def__linha');
  const c = document.createElement('input');
  c.type = 'checkbox';
  c.checked = !!ligado;
  c.onchange = () => aoMudar(c.checked);
  l.append(c);
  const t = elemento('span');
  t.append(elemento('b', null, titulo));
  t.append(elemento('i', null, explica));
  l.append(t);
  return l;
}

function seccao(titulo) {
  const d = elemento('div', 'def__sec');
  d.append(elemento('div', 'members__label', titulo));
  return d;
}

/** O que dizer quando não há nada. Honesto, e com o que falta para haver. */
function aindaNaoHa(oQue, porque) {
  const d = elemento('div', 'def__sec');
  d.append(elemento('div', 'members__label', 'Ainda não existe'));
  const a = elemento('div', 'aviso');
  a.append(elemento('b', null, oQue));
  a.append(document.createTextNode(' ' + porque));
  d.append(a);
  return d;
}

const PAINEIS = {
  conta: {
    nome: 'Conta',
    grupo: 'Definições do utilizador',
    ico: '<circle cx="8" cy="5.6" r="2.8"/><path d="M2.8 14c0-2.9 2.4-4.6 5.2-4.6s5.2 1.7 5.2 4.6"/>',
    desenha: async painel => {
      painel.append(elemento('h2', null, 'Conta'));
      painel.append(elemento('p', null,
        'Não há conta, e-mail nem palavra-passe. A tua identidade é uma chave que foi '
        + 'criada neste computador — ninguém a registou em lado nenhum.'));

      const s1 = seccao('Como apareces');
      const inp = document.createElement('input');
      inp.id = 'def-nome';
      inp.maxLength = 32;
      inp.placeholder = 'o teu nome';
      inp.value = vista.nome || '';
      s1.append(inp);
      const erro = elemento('div', 'caixa__erro');
      erro.id = 'def-erro-nome';
      s1.append(erro);
      const acs = elemento('div', 'caixa__acoes');
      acs.style.justifyContent = 'flex-start';
      const guardar = elemento('button', 'btn btn--primary', 'Guardar');
      const nota = elemento('span', 'nota');
      guardar.onclick = async () => {
        const nome = inp.value.trim();
        if (!nome) { erro.textContent = 'escreve um nome'; return; }
        try {
          await invoke('definir_nome', { nome });
          erro.textContent = '';
          nota.textContent = 'guardado';
          await desenharTudo();
          $('#defs-nome').textContent = nome;
        } catch (e) { erro.textContent = String(e); }
      };
      acs.append(guardar, nota);
      s1.append(acs);
      painel.append(s1);

      const s2 = seccao('A tua chave pública');
      s2.append(elemento('p', 'nota', 'É o teu ID — é por ela que os outros te reconhecem.'));
      const chave = elemento('div', 'def__chave',
        (await invoke('meu_endereco').catch(() => '')) || '—');
      s2.append(chave);
      const a2 = elemento('div', 'caixa__acoes');
      a2.style.justifyContent = 'flex-start';
      const cp = elemento('button', 'btn', 'Copiar a chave');
      cp.onclick = () => navigator.clipboard.writeText(chave.textContent.trim())
        .then(() => { cp.textContent = 'copiada'; setTimeout(() => { cp.textContent = 'Copiar a chave'; }, 1400); })
        .catch(() => {});
      a2.append(cp);
      s2.append(a2);
      painel.append(s2);

      painel.append(seccaoDasPalavras());
    },
  },

  dados: {
    nome: 'Dados e privacidade',
    grupo: 'Definições do utilizador',
    ico: '<path d="M8 1.8 2.8 4v4c0 3.2 2.2 5.6 5.2 6.4 3-.8 5.2-3.2 5.2-6.4V4Z"/>',
    desenha: async painel => {
      painel.append(elemento('h2', null, 'Dados e privacidade'));
      painel.append(elemento('p', null,
        'Não há servidor. Tudo o que escreves vive nesta máquina e nas máquinas de quem '
        + 'está contigo — não há um sítio central onde o histórico se acumule.'));

      const sobre = await invoke('sobre_esta_instalacao').catch(() => null);
      const s1 = seccao('Onde ficam as tuas coisas');
      if (sobre) {
        s1.append(elemento('div', 'def__valor', sobre.pasta));
        s1.append(elemento('p', 'nota',
          'Fora da pasta do programa, para sobreviverem a actualizações e desinstalações.'));
      }
      const a1 = elemento('div', 'caixa__acoes');
      a1.style.justifyContent = 'flex-start';
      const abrir = elemento('button', 'btn', 'Abrir a pasta');
      const nota1 = elemento('span', 'nota');
      abrir.onclick = () => invoke('abrir_pasta_de_dados')
        .catch(e => { nota1.textContent = `não consegui abrir: ${e}`; });
      a1.append(abrir, nota1);
      s1.append(a1);
      painel.append(s1);

      const s2 = seccao('O que a cifra protege — e o que não protege');
      const lista = elemento('div', 'aviso');
      lista.innerHTML =
        '<b>Protege:</b> o que sai desta máquina, e o histórico em disco.<br>'
        + '<b>Não protege:</b> quem já tem a tua <code>identidade.key</code> — para todos os '
        + 'efeitos, essa pessoa é tu.<br>'
        + '<b>Não esconde:</b> quem fala com quem e quando. A isso chama-se metadados.';
      s2.append(lista);
      painel.append(s2);

      const s3 = seccao('Deteção de jogo');
      s3.append(interruptor(
        'Não olhar para as minhas janelas',
        'O rodapé mostra o jogo que tens aberto para o poderes partilhar num clique. '
        + 'Para isso lê os títulos das janelas — nesta máquina, e nunca sai daqui.',
        deteccaoDeJogoDesligada(),
        v => { localStorage.setItem(SEM_JOGO, v ? '1' : '0'); verJogo(); },
      ));
      painel.append(s3);
    },
  },

  permissoes: {
    nome: 'Permissões de mensagens',
    grupo: 'Definições do utilizador',
    ico: '<path d="M3 3.4h10v7.2H6.4L3.4 13Z"/>',
    vazia: true,
    desenha: painel => {
      painel.append(elemento('h2', null, 'Permissões de mensagens'));
      painel.append(aindaNaoHa('Não há sistema de permissões.',
        'Qualquer membro de um servidor pode criar e apagar canais, e não há forma de '
        + 'expulsar ninguém. E há uma razão para isto vir depois e não antes: o convite '
        + 'carrega a chave que decifra o servidor e nunca expira — enquanto essa chave não '
        + 'puder rodar, qualquer expulsão seria teatro, porque o expulso continuaria a '
        + 'decifrar tudo o que fosse escrito a seguir.'));
    },
  },

  notificacoes: {
    nome: 'Notificações',
    grupo: 'Definições do utilizador',
    ico: '<path d="M4.4 6.6a3.6 3.6 0 0 1 7.2 0c0 3 1.2 4 1.2 4H3.2s1.2-1 1.2-4Z"/><path d="M6.6 13a1.6 1.6 0 0 0 2.8 0"/>',
    vazia: true,
    desenha: painel => {
      painel.append(elemento('h2', null, 'Notificações'));
      painel.append(aindaNaoHa('Não há notificações nenhumas.',
        'Nem no sistema, nem som de aviso, nem contagem de não lidas. O caminho para as '
        + 'fazer já existe — a presença de voz viaja por fora do histórico, e é o mesmo '
        + 'molde que uma notificação precisa — mas ainda não foram feitas.'));
    },
  },

  cobrancas: {
    nome: 'Cobranças',
    grupo: 'Definições do utilizador',
    ico: '<rect x="2.4" y="4" width="11.2" height="8" rx="1.6"/><path d="M2.4 6.8h11.2"/>',
    vazia: true,
    desenha: painel => {
      painel.append(elemento('h2', null, 'Cobranças'));
      const d = elemento('div', 'def__sec');
      d.append(elemento('div', 'members__label', 'Não existe, e não vai existir'));
      const a = elemento('div', 'aviso');
      a.append(elemento('b', null, 'Não há nada a pagar.'));
      a.append(document.createTextNode(
        ' Isto não é uma funcionalidade em falta — é o desenho. Não há servidor para '
        + 'sustentar, não há armazenamento a alugar, e não há nada para vender: o Bruma '
        + 'corre entre os teus computadores e mais nada. Esta secção está aqui só para não '
        + 'ficares à procura dela.'));
      d.append(a);
      painel.append(d);
    },
  },

  voz: {
    nome: 'Voz e vídeo',
    grupo: 'Experiência',
    ico: '<rect x="6" y="2" width="4" height="7" rx="2"/><path d="M4 8a4 4 0 0 0 8 0M8 12v2"/>',
    desenha: painel => {
      painel.append(elemento('h2', null, 'Voz e vídeo'));

      const s1 = seccao('Microfone');
      s1.append(interruptor(
        'Supressão de ruído',
        'Tira o ventilador e o teclado, e cancela o eco do que sai das tuas colunas para o '
        + 'teu microfone.',
        ruidoSuprimido,
        () => $('#btn-ruido').click(),
      ));
      painel.append(s1);

      const q = qualidadeDePartilha();
      const s2 = seccao('Partilha de ecrã');
      s2.append(interruptor(
        'Levar o som do sistema',
        'O que sai das tuas colunas segue com a imagem — e a tua própria chamada fica de '
        + 'fora, para ninguém se ouvir a si próprio.',
        q.som,
        v => guardarQualidade({ som: v }),
      ));
      s2.append(elemento('div', 'def__valor',
        `qualidade actual: ${q.altura === 0 ? 'nativa' : q.altura + 'p'} · ${q.fps} ips`
        + `${q.debito ? ' · ' + rotuloDe(OPCOES_DEBITO, q.debito) : ' · débito automático'}`));
      s2.append(elemento('p', 'nota',
        'A resolução e o ritmo escolhem-se na engrenagem, no momento de partilhar — porque '
        + 'a escolha depende do que vais mostrar. E são um TETO: nunca aumentam uma fonte '
        + 'mais pequena do que isso.'));
      painel.append(s2);

      const s3 = seccao('Câmara');
      s3.append(elemento('p', 'nota',
        'Liga-se e desliga-se na barra da chamada. Não há aqui escolha de dispositivo: usa '
        + 'a câmara que o Windows tiver como predefinida.'));
      painel.append(s3);
    },
  },

  aparencia: {
    nome: 'Aparência',
    grupo: 'Experiência',
    ico: '<circle cx="8" cy="8" r="5.6"/><path d="M8 2.4v11.2"/>',
    vazia: true,
    desenha: painel => {
      painel.append(elemento('h2', null, 'Aparência'));
      painel.append(aindaNaoHa('Há um tema só, e é escuro.',
        'A paleta é um azul frio dessaturado — névoa — escolhida de propósito para não ser '
        + 'o roxo do Discord. Um tema claro daria trabalho a fazer bem e ainda não foi '
        + 'feito; um mal feito seria pior do que não haver.'));
    },
  },

  acessibilidade: {
    nome: 'Acessibilidade',
    grupo: 'Experiência',
    ico: '<circle cx="8" cy="3.6" r="1.6"/><path d="M3 6.4h10M8 6.4v4M8 10.4 5.6 14M8 10.4 10.4 14"/>',
    desenha: painel => {
      painel.append(elemento('h2', null, 'Acessibilidade'));
      const s1 = seccao('Movimento');
      s1.append(interruptor(
        'Reduzir o movimento',
        'Pára a névoa do fundo e encurta as transições. A app já respeita a definição do '
        + 'Windows; isto força-a mesmo que o sistema não a peça.',
        localStorage.getItem(SEM_MOVIMENTO) === '1',
        v => { localStorage.setItem(SEM_MOVIMENTO, v ? '1' : '0'); aplicarMovimento(); },
      ));
      painel.append(s1);
    },
  },

  sistema: {
    nome: 'Sistema',
    grupo: 'Experiência',
    ico: '<rect x="2.4" y="3.2" width="11.2" height="7.6" rx="1.4"/><path d="M5.6 13.6h4.8"/>',
    desenha: async painel => {
      painel.append(elemento('h2', null, 'Sistema'));
      const sobre = await invoke('sobre_esta_instalacao').catch(() => null);
      const s1 = seccao('Esta instalação');
      if (sobre) {
        s1.append(elemento('div', 'def__valor', `Bruma ${sobre.versao}`));
        s1.append(elemento('p', 'nota',
          'Quando alguma coisa corre mal, o rasto fica aqui — e é a primeira coisa a olhar:'));
        s1.append(elemento('div', 'def__valor', sobre.registo));
      }
      const a1 = elemento('div', 'caixa__acoes');
      a1.style.justifyContent = 'flex-start';
      const proc = elemento('button', 'btn btn--primary', 'Procurar atualização');
      const nota = elemento('span', 'nota');
      proc.onclick = async () => {
        nota.textContent = 'a procurar…';
        nota.textContent = respostaDaProcura(await procurarAtualizacao());
      };
      const pasta = elemento('button', 'btn', 'Abrir a pasta');
      pasta.onclick = () => invoke('abrir_pasta_de_dados')
        .catch(e => { nota.textContent = `não consegui abrir: ${e}`; });
      a1.append(proc, pasta, nota);
      s1.append(a1);
      painel.append(s1);

      const s2 = seccao('Como se actualiza');
      s2.append(elemento('p', 'nota',
        'A app avisa quando há versão nova e nunca instala sem perguntar. A actualização é '
        + 'assinada, e a assinatura é verificada antes de se escrever seja o que for.'));
      painel.append(s2);
    },
  },

  idioma: {
    nome: 'Idioma e Horário',
    grupo: 'Experiência',
    ico: '<path d="M2.4 5.2h7M5.9 3.4v1.8M4.4 5.2c0 2.6 1.6 4.6 3.6 5.6M7.6 5.2c0 2-1.2 3.6-3 4.6"/><path d="M8.4 13.6 11 7.2l2.6 6.4M9.4 11.6h3.2"/>',
    vazia: true,
    desenha: painel => {
      painel.append(elemento('h2', null, 'Idioma e Horário'));
      painel.append(aindaNaoHa('A app só fala português.',
        'Não há tradução nem escolha de idioma, e as horas seguem o relógio do Windows. '
        + 'Traduzir a app inteira é trabalho a sério, e não valia a pena antes de ela '
        + 'estar assente.'));
    },
  },

  sobreposicao: {
    nome: 'Sobreposição de jogo',
    grupo: 'Jogos e apps',
    ico: '<rect x="2" y="4.4" width="12" height="7.2" rx="2"/><path d="M5 8h2M6 7v2M10.2 8.4h.01M11.6 7h.01"/>',
    vazia: true,
    desenha: painel => {
      painel.append(elemento('h2', null, 'Sobreposição de jogo'));
      painel.append(aindaNaoHa('Não há sobreposição por cima do jogo.',
        'Desenhar por cima de um jogo em ecrã inteiro obriga a entrar no caminho gráfico '
        + 'dele, e é o tipo de coisa que faz jogos fechar e antivírus reclamar. Ainda não '
        + 'foi feito.'));
      const s = seccao('O que existe hoje');
      s.append(elemento('p', 'nota',
        'O Bruma reconhece o jogo que tens aberto e mostra-o no rodapé, para o partilhares '
        + 'num clique. Podes desligar isso em Dados e privacidade.'));
      painel.append(s);
    },
  },
};

const ORDEM = ['conta', 'dados', 'permissoes', 'notificacoes', 'cobrancas',
  'voz', 'aparencia', 'acessibilidade', 'sistema', 'idioma', 'sobreposicao'];

let painelActivo = 'conta';

function desenharMenuDeDefinicoes(filtro = '') {
  const menu = $('#defs-menu');
  menu.textContent = '';
  const procura = filtro.trim().toLowerCase();
  let grupoActual = undefined;
  let algum = false;

  for (const chave of ORDEM) {
    const p = PAINEIS[chave];
    if (procura && !p.nome.toLowerCase().includes(procura)) continue;
    algum = true;
    // Os títulos de grupo só aparecem quando há alguma coisa por baixo deles — a filtrar,
    // um cabeçalho sozinho é ruído.
    if (!procura && p.grupo !== grupoActual) {
      grupoActual = p.grupo;
      if (p.grupo) menu.append(elemento('div', 'defs__grupo', p.grupo));
    }
    const b = elemento('button', 'defs__item' + (p.vazia ? ' is-vazia' : ''));
    b.innerHTML = `<svg viewBox="0 0 16 16" width="15" height="15">${p.ico}</svg>`;
    b.append(document.createTextNode(p.nome));
    b.classList.toggle('is-activa', chave === painelActivo);
    b.onclick = () => mostrarPainel(chave);
    menu.append(b);
  }
  if (!algum) menu.append(elemento('div', 'defs__nada', 'nada com esse nome'));
}

async function mostrarPainel(chave) {
  painelActivo = chave;
  const painel = $('#defs-painel');
  painel.textContent = '';
  await PAINEIS[chave].desenha(painel);
  painel.scrollTop = 0;
  $('#defs-conteudo').scrollTop = 0;   // secção nova começa no topo, e não a meio da anterior
  desenharMenuDeDefinicoes($('#defs-buscar').value);
}

async function abrirDefinicoes(qual) {
  const d = $('#defs');
  d.hidden = false;
  $('#defs-nome').textContent = vista.nome || '—';
  pintar($('#defs-avatar'), vista.eu || '');
  $('#defs-buscar').value = '';
  await mostrarPainel(qual || 'conta');
}

function fecharDefinicoes() { $('#defs').hidden = true; }

$('#defs-fechar').onclick = fecharDefinicoes;
$('#defs-editar').onclick = () => mostrarPainel('conta');
$('#defs-buscar').oninput = () => desenharMenuDeDefinicoes($('#defs-buscar').value);
document.addEventListener('keydown', ev => {
  if (ev.key === 'Escape' && !$('#defs').hidden) fecharDefinicoes();
});

/** O que dizer depois de procurar uma versão nova.
 *
 *  Está à parte por ser a única forma de provar, sem rede, que as três respostas são
 *  mesmo três -- e não duas com a falha disfarçada de boa notícia. */
function respostaDaProcura(r) {
  if (r === 'ha') return '';                       // a faixa já apareceu e diz tudo
  if (r === 'nao') return 'já estás na versão mais recente';
  return 'não consegui verificar — sem rede, ou o servidor está em baixo';
}

/** A secção das 24 palavras. Vive na Conta, que é onde alguém a iria procurar. */
function seccaoDasPalavras() {
  const d = seccao('As tuas 24 palavras');
  d.append(elemento('p', 'nota',
    'São a tua chave escrita de outra maneira. Com elas recuperas a identidade noutra '
    + 'máquina, ou nesta depois de formatar.'));
  const av = elemento('div', 'aviso');
  av.append(elemento('b', null, 'Quem tiver estas palavras é tu.'));
  av.append(document.createTextNode(
    ' Lê o teu histórico, entra nas tuas salas e fala em teu nome. Escreve-as num papel e '
    + 'guarda-o onde guardarias uma chave de casa.'));
  d.append(av);

  const caixa = elemento('div', 'palavras');
  caixa.hidden = true;
  d.append(caixa);
  const nota = elemento('span', 'nota');

  const acs = elemento('div', 'caixa__acoes');
  acs.style.justifyContent = 'flex-start';
  const ver = elemento('button', 'btn btn--primary', 'Mostrar as palavras');
  const copiar = elemento('button', 'btn', 'Copiar');
  copiar.hidden = true;
  ver.onclick = async () => {
    try {
      const texto = await invoke('palavras_da_identidade');
      caixa.textContent = '';
      texto.split(/\s+/).forEach((palavra, n) => {
        const linha = elemento('span');
        linha.append(elemento('i', null, String(n + 1).padStart(2, ' ')));
        linha.append(document.createTextNode(palavra));
        caixa.append(linha);
      });
      caixa.hidden = false;
      ver.hidden = true;
      copiar.hidden = false;
      nota.textContent = '';
    } catch (e) { nota.textContent = `não consegui: ${e}`; }
  };
  copiar.onclick = () => {
    const texto = [...caixa.children].map(l => l.textContent.replace(/^\s*\d+\s*/, '')).join(' ');
    navigator.clipboard.writeText(texto)
      .then(() => { nota.textContent = 'copiadas — cola-as num sítio só teu, e apaga a seguir'; })
      .catch(() => { nota.textContent = 'não consegui copiar; escreve-as à mão'; });
  };
  const restaurar = elemento('button', 'btn', 'Restaurar de outras palavras');
  acs.append(ver, copiar, restaurar, nota);
  d.append(acs);

  const cx = elemento('div');
  cx.hidden = true;
  const ta = document.createElement('textarea');
  ta.id = 'palavras-entrada';
  ta.rows = 3;
  ta.placeholder = 'palavra1 palavra2 palavra3 …';
  cx.append(ta);
  const perigo = elemento('div', 'aviso aviso--perigo');
  perigo.innerHTML = 'Isto <b>troca</b> a identidade desta máquina. Os servidores que tens '
    + 'aqui deixam de abrir — as chaves deles pertencem à identidade antiga. Nada é apagado: '
    + 'o índice antigo fica guardado ao lado, e voltas a entrar nas salas por convite.';
  cx.append(perigo);
  const a2 = elemento('div', 'caixa__acoes');
  a2.style.justifyContent = 'flex-start';
  const fazer = elemento('button', 'btn btn--perigo', 'Restaurar');
  const nota2 = elemento('span', 'nota');
  nota2.id = 'restaurar-nota';
  fazer.onclick = async () => {
    const palavras = ta.value.trim();
    if (!palavras) { nota2.textContent = 'escreve as 24 palavras primeiro'; return; }
    nota2.textContent = 'a verificar…';
    try {
      nota2.textContent = await invoke('restaurar_identidade', { palavras });
      fazer.disabled = true;
      ta.disabled = true;
    } catch (e) { nota2.textContent = String(e); }
  };
  a2.append(fazer, nota2);
  cx.append(a2);
  d.append(cx);
  restaurar.onclick = () => { cx.hidden = !cx.hidden; if (!cx.hidden) ta.focus(); };
  return d;
}

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

/* ==========================================================================
   A voz, pelo mesmo caminho do ecrã.

   Isto ia por WebRTC, e o WebRTC precisa que alguém lhe diga por onde furar o router —
   um servidor STUN ou TURN, configurado à mão nas duas máquinas. Sem isso ele só encontra
   caminhos dentro da rede local, e entre duas casas não há nenhum. Era a última coisa no
   Bruma que exigia configuração para funcionar de todo.

   Agora o som é codificado aqui em Opus, entregue ao Rust, e vai pelo iroh — que já trata
   do NAT sozinho e já é o caminho das mensagens e do ecrã. Não há nada para configurar, e
   a voz deixa também de expor o endereço de quem fala.
   ========================================================================== */

const VOZ_HZ = 48000;
const VOZ_BITRATE = 24000;      // Opus a 24 kbps é transparente para fala
const VOZ_QUADRO_US = 20000;    // 20 ms por pedaço

/** Quanto som se guarda antes de o tocar.
 *
 *  A rede não entrega os pedaços com o espaçamento com que eles saíram: uns chegam
 *  atrasados, outros aos pares. Tocar cada um assim que chega dá estalidos. Guardam-se 80
 *  ms de folga — o suficiente para absorver a irregularidade normal, pouco o bastante para
 *  não se notar na conversa.
 */
const VOZ_FOLGA = 0.08;

let vozCtx = null;
function contextoDeAudio() {
  if (!vozCtx) vozCtx = new AudioContext({ sampleRate: VOZ_HZ });
  if (vozCtx.state === 'suspended') vozCtx.resume();
  return vozCtx;
}

/* --- enviar ---------------------------------------------------------------- */

let envio = null;

/** Porque é que a tua voz não está a sair, se não está.
 *
 *  A câmara já tinha isto (`camaraFalhou`); a voz não tinha, e é a mais grave das duas:
 *  continuas na sala, com o microfone aceso e a aparecer presente, e **ninguém te ouve**.
 *  Sem uma palavra em lado nenhum, o sintoma do outro lado é "ele hoje está calado".
 */
let vozFalhou = null;

/** De quem é que deixaste de ouvir, e porquê. Um descodificador que morre tira-te UMA
 *  pessoa da chamada — ela continua a aparecer, e a suspeita cai nela. */
const vozPartida = new Map();

function comecarAEnviarVoz(microfone) {
  pararDeEnviarVoz();
  const faixa = microfone && microfone.getAudioTracks()[0];
  if (!faixa || typeof MediaStreamTrackProcessor === 'undefined') return;

  let carimbo = 0;
  const codificador = new AudioEncoder({
    output: pedaco => {
      // Só se envia a quem está mesmo na sala. Falar para uma lista vazia não custa nada
      // e não se manda nada para lado nenhum.
      const gente = [...voz.presentes.entries()]
        .filter(([, c]) => c === voz.canal).map(([p]) => p);
      if (!gente.length) return;
      const bytes = new Uint8Array(pedaco.byteLength);
      pedaco.copyTo(bytes);
      invoke('enviar_voz', { para: gente, dados: [...bytes] }).catch(() => {});
    },
    error: e => {
      console.warn('o codificador de voz parou:', e);
      vozFalhou = 'O codificador de voz desistiu — ninguém te ouve. Sai e volta a entrar.';
      pararDeEnviarVoz();
      desenharRodape();
      desenharVoz();
    },
  });
  codificador.configure({
    codec: 'opus',
    sampleRate: VOZ_HZ,
    numberOfChannels: 1,
    bitrate: VOZ_BITRATE,
    opus: { frameDuration: VOZ_QUADRO_US },
  });

  const leitor = new MediaStreamTrackProcessor({ track: faixa }).readable.getReader();
  envio = { codificador, leitor, vivo: true };

  (async () => {
    while (envio && envio.vivo) {
      const { value, done } = await leitor.read().catch(() => ({ done: true }));
      if (done) break;
      // O microfone silenciado não envia nada. Não basta baixar o volume: o que não sai
      // desta máquina é o que ninguém pode ouvir.
      const calado = !faixa.enabled;
      if (!calado && codificador.state === 'configured') {
        try { codificador.encode(value); } catch (e) { /* o próximo vai */ }
      }
      carimbo = value.timestamp;
      value.close();
    }
  })();
  void carimbo;
}

function pararDeEnviarVoz() {
  if (!envio) return;
  envio.vivo = false;
  try { envio.leitor.cancel(); } catch (e) { /* já fechado */ }
  try { if (envio.codificador.state !== 'closed') envio.codificador.close(); } catch (e) { /* idem */ }
  envio = null;
}

/* ==========================================================================
   A câmara.

   Vai pelo caminho da voz — codificada aqui, com WebCodecs — e não pelo do ecrã, que é
   captado em Rust. A razão é a barra: o `getDisplayMedia` faz o WebView2 desenhar "está a
   partilhar", e foi por isso que o ecrã teve de sair do navegador. O `getUserMedia` de uma
   CÂMARA não faz isso — é só para captura de ecrã. Portanto a câmara pode ficar aqui, onde
   já existe tudo o que ela precisa: abrir dispositivos, codificar e desenhar.

   E há uma segunda razão, mais forte: o ecrã é UM de cada vez e enche o painel; as câmaras
   são VÁRIAS ao mesmo tempo. Descodificar N fluxos em paralelo é coisa que o navegador faz
   sozinho e que teria de ser reescrita à mão do outro lado.
   ========================================================================== */

const CAM_LARGURA = 640;
const CAM_ALTURA = 360;
const CAM_IPS = 24;
const CAM_DEBITO = 400_000;      // 400 kbps chega para uma cara a 360p
/** De quanto em quanto tempo se manda um frame COMPLETO.
 *
 *  Os outros frames só descrevem o que mudou desde o anterior, portanto quem chega a meio
 *  não consegue descodificar nada até vir um completo. Dois segundos é o compromisso: mais
 *  curto gasta upload à toa, mais longo deixa quem entra a olhar para um quadrado preto.
 */
const CAM_CHAVE_MS = 2000;

let camaraEnvio = null;
/** Conta quantas vezes se ligou ou desligou a câmara.
 *
 *  Serve para uma corrida real: entre carregar no botão e o `getUserMedia` responder passam
 *  centenas de milissegundos, e nesse intervalo a pessoa pode sair da chamada. Sem isto, o
 *  stream chegava depois e ficava ligado — com a luz da câmara acesa e ninguém em sala.
 *  Uma luz de câmara acesa sem chamada é a pior coisa que esta app podia fazer.
 */
let geracaoDaCamara = 0;

/** Uma câmara desenhada por nós, para o teste de par.
 *
 *  Existe porque a máquina onde isto se desenvolve não tem câmara nenhuma — só uma virtual
 *  do OBS, que não arranca sem o OBS aberto. Sem uma fonte destas, o caminho da câmara
 *  entre duas instâncias ficava por provar à espera de hardware, que é a pior razão para
 *  deixar código por verificar.
 *
 *  O quadrado ANDA de propósito: uma imagem parada comprime para quase nada e provaria
 *  pouco — o que interessa é que saiam bytes a sério e cheguem inteiros ao outro lado.
 */
function camaraDesenhada() {
  const tela = document.createElement('canvas');
  tela.width = CAM_LARGURA;
  tela.height = CAM_ALTURA;
  const pincel = tela.getContext('2d');
  let x = 0;
  const pintar = () => {
    pincel.fillStyle = '#101418';
    pincel.fillRect(0, 0, tela.width, tela.height);
    pincel.fillStyle = '#7fd4c1';
    pincel.fillRect(x % (tela.width - 60), 40 + (x % 200), 60, 60);
    x += 11;
  };
  const relogio = setInterval(pintar, 1000 / CAM_IPS);
  pintar();
  const stream = tela.captureStream(CAM_IPS);
  stream.__parar = () => clearInterval(relogio);
  return stream;
}

/** Quem está na sala E percebe o que vamos mandar.
 *
 *  Mandar a câmara a quem não a sabe distinguir do ecrã não é "melhor do que nada": é pior.
 *  Aquele lado meteria os bytes no descodificador errado e mostraria lixo, sem saber porquê.
 *  Vazia significa que nada sai da máquina — que é a resposta certa quando ninguém percebe.
 */
function gentePresente() {
  return [...voz.presentes.entries()]
    .filter(([p, c]) => c === voz.canal && voz.entendeCamara.has(p))
    .map(([p]) => p);
}

async function comecarAEnviarCamara(fonte = null) {
  pararDeEnviarCamara();
  if (typeof VideoEncoder === 'undefined' || typeof MediaStreamTrackProcessor === 'undefined') {
    throw new Error('esta versão do WebView2 não traz o codificador de vídeo');
  }
  const minhaVez = ++geracaoDaCamara;
  const stream = fonte || await navigator.mediaDevices.getUserMedia({
    video: { width: CAM_LARGURA, height: CAM_ALTURA, frameRate: CAM_IPS },
    audio: false,
  });
  // Chegou tarde: alguém desligou a câmara ou saiu da chamada enquanto isto esperava.
  // Apaga-se a luz e desiste-se em silêncio — não é um erro, é uma decisão que mudou.
  if (minhaVez !== geracaoDaCamara) {
    stream.getTracks().forEach(t => t.stop());
    if (stream.__parar) stream.__parar();
    return null;
  }
  const faixa = stream.getVideoTracks()[0];
  if (!faixa) { stream.getTracks().forEach(t => t.stop()); throw new Error('sem câmara'); }

  let ultimaChave = 0;
  const codificador = new VideoEncoder({
    output: pedaco => {
      const gente = gentePresente();
      if (!gente.length) return;
      const bytes = new Uint8Array(pedaco.byteLength);
      pedaco.copyTo(bytes);
      invoke('enviar_camara', {
        para: gente, servidor: voz.servidor, canal: voz.canal, dados: [...bytes],
      }).catch(() => {});
    },
    error: e => console.warn('o codificador da câmara parou:', e),
  });
  codificador.configure({
    codec: 'avc1.42001f',            // Baseline 3.1 — o que qualquer máquina descodifica
    width: CAM_LARGURA,
    height: CAM_ALTURA,
    framerate: CAM_IPS,
    bitrate: CAM_DEBITO,
    latencyMode: 'realtime',
    // `annexb` e não `avc`: em annexb cada pedaço traz os seus próprios parâmetros e
    // descodifica-se sozinho. Em `avc` os parâmetros vão UMA vez, fora da banda, e quem
    // entrasse a meio da chamada nunca mais os via — o mesmo problema que o `tfdt`
    // resolveu no ecrã, e a mesma lição: numa transmissão, cada pedaço tem de se bastar.
    avc: { format: 'annexb' },
  });

  const leitor = new MediaStreamTrackProcessor({ track: faixa }).readable.getReader();
  camaraEnvio = { codificador, leitor, faixa, stream, vivo: true };

  (async () => {
    while (camaraEnvio && camaraEnvio.vivo) {
      const { value, done } = await leitor.read().catch(() => ({ done: true }));
      if (done) break;
      // Um codificador que morreu não volta. Continuar a puxar frames seria gastar CPU e
      // manter a luz acesa para não enviar nada a ninguém — e em silêncio, que é o pior.
      if (codificador.state !== 'configured') {
        value.close();
        console.warn('o codificador da câmara morreu; a desligar');
        camaraFalhou = 'A câmara parou sozinha — o codificador de vídeo desistiu.';
        pararDeEnviarCamara();
        anunciarEstado();
        desenharVoz();
        desenharRodape();
        break;
      }
      {
        const agora = performance.now();
        const chave = agora - ultimaChave >= CAM_CHAVE_MS;
        if (chave) ultimaChave = agora;
        // Se a fila cresce, o codificador não está a acompanhar. Largar o frame é melhor
        // do que o acumular: numa chamada ao vivo o que interessa é o presente.
        if (codificador.encodeQueueSize < 3) {
          try { codificador.encode(value, { keyFrame: chave }); } catch (e) { /* o próximo vai */ }
        }
      }
      value.close();
    }
  })();

  voz.camara = stream;
  return stream;
}

function pararDeEnviarCamara() {
  // Sobe SEMPRE, mesmo sem nada a parar: é o que faz um `getUserMedia` ainda a decorrer
  // perceber que já não é preciso.
  geracaoDaCamara += 1;
  if (!camaraEnvio) return;
  camaraEnvio.vivo = false;
  try { camaraEnvio.leitor.cancel(); } catch (e) { /* já fechado */ }
  try {
    if (camaraEnvio.codificador.state !== 'closed') camaraEnvio.codificador.close();
  } catch (e) { /* idem */ }
  // A luz da câmara só se apaga quando a faixa PARA. Deixar o stream vivo com o
  // codificador fechado seria a pior das combinações: não sai nada e a luz fica acesa.
  try { camaraEnvio.stream.getTracks().forEach(t => t.stop()); } catch (e) { /* idem */ }
  try { if (camaraEnvio.stream.__parar) camaraEnvio.stream.__parar(); } catch (e) { /* idem */ }
  camaraEnvio = null;
  voz.camara = null;
}

/* --- receber a câmara dos outros ------------------------------------------- */

/** Um descodificador por pessoa, e um <video> que se reaproveita entre redesenhos. */
const camarasRecebidas = new Map();

function camaraDe(chave) {
  let c = camarasRecebidas.get(chave);
  if (c) return c;

  const tela = document.createElement('canvas');
  tela.className = 'tile__video';
  tela.width = CAM_LARGURA;
  tela.height = CAM_ALTURA;
  const pincel = tela.getContext('2d');

  c = { tela, pincel, descodificador: null, frames: 0, esperaChave: true };
  c.descodificador = new VideoDecoder({
    output: quadro => {
      c.frames += 1;
      if (tela.width !== quadro.displayWidth || tela.height !== quadro.displayHeight) {
        tela.width = quadro.displayWidth;
        tela.height = quadro.displayHeight;
      }
      try { pincel.drawImage(quadro, 0, 0, tela.width, tela.height); } catch (e) { /* segue */ }
      quadro.close();
    },
    error: e => {
      console.warn('descodificador da câmara de', chave, e);
      // Um erro deixa o descodificador inutilizável: exige-se um frame completo antes de
      // se lhe voltar a dar seja o que for, senão entra num ciclo de queixas.
      c.esperaChave = true;
    },
  });
  c.descodificador.configure({ codec: 'avc1.42001f', optimizeForLatency: true });
  camarasRecebidas.set(chave, c);
  return c;
}

(function ligarEntradaDeCamara() {
  if (!window.__TAURI__) return;
  const canal = new window.__TAURI__.core.Channel();
  canal.onmessage = pedaco => {
    const bytes = new Uint8Array(pedaco);
    if (!bytes.length) return;
    const n = bytes[0];
    if (bytes.length < 1 + n) return;
    const chave = new TextDecoder().decode(bytes.subarray(1, 1 + n));
    const corpo = bytes.subarray(1 + n);
    const c = camaraDe(chave);
    if (c.descodificador.state !== 'configured') return;

    // Em annexb, um frame completo traz o SPS (nal 7) à frente. Quem chega a meio tem de
    // esperar por um; dar-lhe um frame de diferenças é pedir imagem partida.
    const completo = temSPS(corpo);
    if (c.esperaChave) {
      if (!completo) return;
      c.esperaChave = false;
    }
    const primeiro = c.frames === 0;
    try {
      c.descodificador.decode(new EncodedVideoChunk({
        type: completo ? 'key' : 'delta',
        timestamp: performance.now() * 1000,
        data: corpo,
      }));
    } catch (e) {
      c.esperaChave = true;
      return;
    }
    if (primeiro) desenharVoz();   // o painel só sabe que há imagem depois do primeiro
  };
  invoke('receber_camara', { canal }).catch(() => {});
})();

/** Se este pedaço traz um SPS — ou seja, se é um frame que se descodifica sozinho.
 *
 *  Procura-se o código de início (00 00 01) e olha-se para os 5 bits baixos do byte
 *  seguinte: 7 é SPS. É mais barato e mais fiável do que confiar no que o codificador
 *  disse, porque o que chega ao outro lado são só bytes.
 */
function temSPS(bytes) {
  for (let i = 0; i + 3 < bytes.length; i++) {
    if (bytes[i] === 0 && bytes[i + 1] === 0 && bytes[i + 2] === 1) {
      const tipo = bytes[i + 3] & 0x1f;
      if (tipo === 7) return true;
      if (tipo === 1 || tipo === 5) return tipo === 5;
    }
  }
  return false;
}

/** Quem desligou a câmara deixa de ter descodificador. Não é só arrumação: um
 *  descodificador aberto continua a segurar memória de vídeo, e numa sala onde as pessoas
 *  vão ligando e desligando isso só cresce. */
/** A própria imagem, sem passar pela rede.
 *
 *  Espelhada, como em toda a gente: a pessoa está a ver-se a si, e ver-se ao contrário do
 *  espelho do quarto é desconcertante. Quem recebe vê a imagem na posição certa — a
 *  inversão é só local, no CSS.
 */
let espelho = null;
function meuEspelho() {
  if (!voz.camara) {
    if (espelho) { espelho.srcObject = null; espelho = null; }
    return null;
  }
  if (!espelho) {
    espelho = document.createElement('video');
    espelho.className = 'tile__video tile__video--espelho';
    espelho.autoplay = true;
    espelho.muted = true;      // é a nossa própria câmara; não tem som e não se ouve
    espelho.playsInline = true;
  }
  if (espelho.srcObject !== voz.camara) espelho.srcObject = voz.camara;
  return espelho;
}

function fecharCamaraRecebida(chave) {
  const c = camarasRecebidas.get(chave);
  if (!c) return;
  try {
    if (c.descodificador.state !== 'closed') c.descodificador.close();
  } catch (e) { /* já fechado */ }
  camarasRecebidas.delete(chave);
}

/* --- receber --------------------------------------------------------------- */

function vozDe(chave) {
  let v = voz.audio.get(chave);
  if (v) return v;

  const ctx = contextoDeAudio();
  const ganho = ctx.createGain();
  ganho.connect(ctx.destination);

  v = { ganho, proximo: 0, descodificador: null, ctx };
  v.descodificador = new AudioDecoder({
    output: som => tocar(chave, som),
    error: e => {
      console.warn('descodificador de voz de', chave, e);
      vozPartida.set(chave, 'o áudio desta pessoa não está a descodificar');
      desenharVoz();
    },
  });
  v.descodificador.configure({ codec: 'opus', sampleRate: VOZ_HZ, numberOfChannels: 1 });
  voz.audio.set(chave, v);
  ajustarVolume(chave);
  return v;
}

function tocar(chave, som) {
  const v = voz.audio.get(chave);
  if (!v) { som.close(); return; }
  const ctx = v.ctx;

  const amostras = new Float32Array(som.numberOfFrames);
  try {
    som.copyTo(amostras, { planeIndex: 0, format: 'f32-planar' });
  } catch (e) {
    som.close();
    return;
  }
  som.close();

  // O anel verde de quem fala sai daqui: já se está a olhar para as amostras, não vale a
  // pena montar um analisador em paralelo só para as medir outra vez.
  medirNasAmostras(chave, amostras);

  const buffer = ctx.createBuffer(1, amostras.length, VOZ_HZ);
  buffer.copyToChannel(amostras, 0);
  const fonte = ctx.createBufferSource();
  fonte.buffer = buffer;
  fonte.connect(v.ganho);

  const agora = ctx.currentTime;
  // Se ficámos para trás (a app esteve minimizada, a rede engasgou), não se tenta
  // recuperar o atraso a tocar tudo de enfiada: numa conversa ao vivo o que interessa é o
  // presente. Recomeça-se com a folga normal.
  if (v.proximo < agora + 0.01 || v.proximo > agora + 0.6) v.proximo = agora + VOZ_FOLGA;
  fonte.start(v.proximo);
  v.proximo += buffer.duration;
}

function calarPeer(chave) {
  const v = voz.audio.get(chave);
  if (!v) return;
  try { if (v.descodificador.state !== 'closed') v.descodificador.close(); } catch (e) { /* já */ }
  try { v.ganho.disconnect(); } catch (e) { /* já */ }
  voz.audio.delete(chave);
  voz.falando.delete(chave);
}

/** O volume de uma pessoa: zero se estivermos surdos ou se ela estiver silenciada. */
function ajustarVolume(chave) {
  const v = voz.audio.get(chave);
  if (!v) return;
  v.ganho.gain.value = (surdo || voz.silenciados.has(chave)) ? 0 : 1;
}

function ajustarTodosOsVolumes() {
  for (const chave of voz.audio.keys()) ajustarVolume(chave);
}

/* Os pedaços chegam do Rust com a chave de quem falou à frente. */
(function ligarEntradaDeVoz() {
  if (!window.__TAURI__) return;
  const canal = new window.__TAURI__.core.Channel();
  canal.onmessage = pedaco => {
    const bytes = pedaco instanceof ArrayBuffer ? new Uint8Array(pedaco) : new Uint8Array(pedaco);
    if (!bytes.length) return;
    const n = bytes[0];
    if (bytes.length < 1 + n) return;
    const chave = new TextDecoder().decode(bytes.subarray(1, 1 + n));
    if (!voz.canal) return;                 // não estamos numa sala: ignora-se
    const v = vozDe(chave);
    if (v.descodificador.state !== 'configured') return;
    try {
      v.descodificador.decode(new EncodedAudioChunk({
        type: 'key',                        // no Opus todos os pedaços se bastam a si
        timestamp: performance.now() * 1000,
        data: bytes.subarray(1 + n),
      }));
    } catch (e) { /* um pedaço perdido não vale um erro */ }
  };
  invoke('receber_voz', { canal }).catch(() => {});
})();

/* ---------- eventos vindos do núcleo ---------- */

listen('servidor-mudou', async ev => {
  await desenharTudo();
  // O chat da sala vive na coluna da direita, fora da vista de canal: se estivermos a
  // ler um canal de texto, o desenharTudo não lhe toca e as mensagens novas não apareciam.
  await desenharChatDaSala();
});
listen('peer-ligado', () => { ligados += 1; desenharTopo(); });
listen('peer-desligado', () => { ligados = Math.max(0, ligados - 1); desenharTopo(); });

/* ---------- explicações: o porquê vive na app ---------- */

const EXPLICACOES = {
  identidade: {
    titulo: 'A tua identidade',
    corpo: [
      'Foi criada neste computador na primeira vez que abriste a app. É uma chave, e é ao mesmo tempo o teu ID e o teu endereço na rede.',
      'Não existe conta nem registo. Mas existem <b>24 palavras</b> que a recuperam noutra máquina — se as guardares antes de precisares delas.',
    ],
    accao: { rotulo: 'Ver as minhas 24 palavras', abre: 'veu-definicoes' },
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
  'chat-voz': {
    titulo: 'O chat desta sala',
    corpo: [
      'É um canal à parte dos canais de texto, e só aparece enquanto estiveres na sala. O histórico fica: sais, voltas, e continua lá.',
      '<b>Esconder não é o mesmo que cifrar.</b> Isto é uma regra desta app: a mensagem viaja com a chave do servidor, igual a todas as outras, por isso chega ao computador de todos os membros. Um cliente modificado conseguia lê-la sem entrar na sala.',
      'Para ser garantia a sério, a sala precisava de chave própria — e ainda não tem.',
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
  if (e.accao) {
    const b = elemento('button', 'btn btn--primary', e.accao.rotulo);
    b.onclick = () => { esconderExplicacao(); abrirDefinicoes(); };
    corpo.append(b);
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

/* --------------------------------------------------------------------------
   Atualizações.

   O plugin sozinho não faz nada — é preciso alguém perguntar. E a atualização
   nunca se instala em silêncio: quem está a usar a app decide quando reinicia,
   porque reiniciar a meio de uma conversa é uma coisa que se faz a alguém.
   -------------------------------------------------------------------------- */

/** Procura uma versão nova.
 *
 *  Devolve `'ha'`, `'nao'` ou `'falhou'` -- três respostas e não duas, porque «não há
 *  versão nova» e «não consegui saber» são coisas diferentes para quem carregou no botão.
 *  Enquanto isto devolvia `false` nos dois casos, as Definições diziam "já estás na versão
 *  mais recente" a alguém que estava sem rede. */
async function procurarAtualizacao() {
  try {
    const { check } = window.__TAURI__.updater;
    const nova = await check();
    if (!nova) return 'nao';
    $('#texto-update').textContent = `Há uma versão nova do Bruma (${nova.version}).`;
    $('#faixa-update').hidden = false;
    $('#adiar-update').onclick = () => { $('#faixa-update').hidden = true; };
    $('#btn-update').onclick = async () => {
      $('#btn-update').disabled = true;
      $('#texto-update').textContent = 'A descarregar…';
      try {
        // A ultima linha que a app escreve antes de se fechar sozinha. Se o registo acabar
        // aqui, a instalacao nao chegou ao fim -- e o instalador continua a escrever nele.
        await invoke('capacidades', { linha: 'a instalar a actualizacao...' }).catch(() => {});
        await nova.downloadAndInstall();
        // Código morto, e fica escrito porquê: o `downloadAndInstall` acima chama
        // `exit(0)` assim que lança o instalador, portanto esta linha nunca corre. O que
        // relança a app é o próprio instalador, no fim.
        await window.__TAURI__.process.relaunch();
      } catch (e) {
        $('#texto-update').textContent = `Não consegui atualizar: ${e}`;
        $('#btn-update').disabled = false;
      }
    };
    return 'ha';
  } catch (e) {
    // Sem rede, ou o endpoint em baixo. No arranque não vale a pena incomodar ninguém —
    // por isso é que isto não atira. Mas quem carregou no botão tem de saber a diferença.
    console.warn('verificação de atualização falhou:', e);
    return 'falhou';
  }
}

/* ==========================================================================
   Menu de contexto próprio.

   O menu do WebView2 oferece "Guardar como", "Imprimir", "Enviar a guia para os
   teus dispositivos" e "Inspecionar" — vocabulário de browser, não de aplicação.
   Suprime-se e põe-se um que fale das coisas que existem aqui.
   ========================================================================== */

const menu = $('#menu');

function abrirMenu(x, y, itens) {
  menu.textContent = '';
  for (const it of itens) {
    if (it === '-') { menu.append(document.createElement('hr')); continue; }
    const b = elemento('button', it.perigo ? 'perigo' : null, it.rotulo);
    b.onclick = () => { menu.hidden = true; it.accao(); };
    menu.append(b);
  }
  menu.hidden = false;
  // Encostar ao rato, mas nunca sair do ecrã.
  const l = Math.min(x, innerWidth - menu.offsetWidth - 8);
  const t = Math.min(y, innerHeight - menu.offsetHeight - 8);
  menu.style.left = `${Math.max(8, l)}px`;
  menu.style.top = `${Math.max(8, t)}px`;
}

document.addEventListener('contextmenu', ev => {
  ev.preventDefault();          // <- é isto que mata o menu do browser
  const itens = [];

  const msg = ev.target.closest('.msg');
  const canal = ev.target.closest('.chan');
  const membro = ev.target.closest('.member');
  const seleccao = String(getSelection()).trim();

  if (seleccao) {
    itens.push({ rotulo: 'Copiar', accao: () => navigator.clipboard.writeText(seleccao) });
  }
  if (msg && !seleccao) {
    const p = msg.querySelector('p');
    const texto = p ? p.textContent : '';
    itens.push({ rotulo: 'Copiar mensagem', accao: () => navigator.clipboard.writeText(texto) });
  }
  if (membro && membro.dataset.chave) {
    const chave = membro.dataset.chave;
    itens.push({ rotulo: 'Copiar chave', accao: () => navigator.clipboard.writeText(chave) });
  }
  if (canal && canal.dataset.canal) {
    const id = canal.dataset.canal;
    if (itens.length) itens.push('-');
    itens.push({
      rotulo: 'Apagar canal', perigo: true,
      accao: () => invoke('apagar_canal', { servidor: servidorAtual, canal: id }).catch(console.error),
    });
  }
  if (servidorAtual && !canal && !msg && !membro) {
    itens.push({ rotulo: 'Convidar alguém', accao: () => $('#btn-convite').click() });
  }
  if (itens.length) itens.push('-');
  itens.push({ rotulo: 'Como isto se liga…', accao: abrirDefinicoesDeRede });

  abrirMenu(ev.clientX, ev.clientY, itens);
});

document.addEventListener('click', () => { menu.hidden = true; }, true);
document.addEventListener('keydown', ev => { if (ev.key === 'Escape') menu.hidden = true; });

/* ==========================================================================
   Voz e partilha de ecrã.

   A sinalização vai por cima do iroh, que já resolveu o NAT para o chat. O WebRTC
   faz o seu próprio caminho para a média, e por isso pode precisar de TURN — daí
   as definições de ligação.
   ========================================================================== */

const voz = {
  eu: null,
  servidor: null,
  canal: null,
  micro: null,
  ecra: null,
  camara: null,
  audio: new Map(),      // peer -> como se lhe ouve a voz
  presentes: new Map(),  // peer -> canal em que está
  falando: new Set(),    // quem está a emitir som agora
  silenciados: new Set(),// pessoas silenciadas uma a uma
  aPartilhar: new Set(), // quem está a transmitir o ecrã
  comCamara: new Set(),  // quem tem a câmara ligada
  /** O que cada transmissor diz sobre a transmissão dele: qualidade e quantos o veem. */
  infoDaTransmissao: new Map(),
  /** O tamanho com que a MINHA captura ficou mesmo, devolvido pelo Rust. */
  ecraTamanho: null,
  /** A qualidade que está MESMO a correr, congelada quando a partilha começou.
   *
   *  Sem isto, o rótulo lia o menu no momento de desenhar — e quem mexesse na engrenagem a
   *  meio via a barra a anunciar números que ninguém estava a usar. Mudar a qualidade só
   *  vale para transmissões novas, e a barra tem de contar a mesma história. */
  qualidadeEmUso: null,
  // Quem, do outro lado, percebe o que esta versão envia. Ver PROTOCOLO.
  entendeCamara: new Set(),
  entendeSom: new Set(),
  // De quem já recebemos um anúncio de estado. Sem isto não se distingue "é antigo" de
  // "ainda não disse nada", e as duas coisas mereciam respostas opostas.
  jaFalou: new Set(),
  aVer: null,            // de quem estou a ver a transmissão
  aSerVistoPor: new Set(), // quem pediu para ver o MEU ecrã — só a esses se envia
  vejoMeuEcra: null,       // o <video> da minha própria transmissão, só enquanto olho
  analisadores: new Map(),
  audioCtx: null,
};

/** Já não há definições de rede — este painel passou a explicar porque é que não há, e a
 *  mostrar o que está mesmo a acontecer.
 *
 *  O que está aqui é o que transforma um "não se ouve nada" numa resposta: se saíram
 *  pacotes e não entrou nenhum, o problema é do outro lado; se não saiu nenhum, é deste;
 *  se entraram e saíram e mesmo assim não se ouve, o problema não é a rede. São três
 *  sítios diferentes, e sem isto escolhe-se um à sorte.
 */
function abrirDefinicoesDeRede() {
  abrir('veu-rede');
  desenharDiagnostico();
}

let relogioDiag = null;
async function desenharDiagnostico() {
  const alvo = $('#diag-rede');
  if (!alvo) return;
  if ($('#veu-rede').hidden) {
    if (relogioDiag) { clearInterval(relogioDiag); relogioDiag = null; }
    return;
  }
  if (!relogioDiag) relogioDiag = setInterval(desenharDiagnostico, 1500);

  const gente = [...voz.presentes.keys()];
  if (!gente.length) {
    alvo.textContent = 'Ninguém ligado neste momento.';
    return;
  }
  const estado = await invoke('qualidade', { peers: gente }).catch(() => []);
  alvo.textContent = '';
  if (!estado.length) {
    alvo.textContent = `${gente.length} presente(s), nenhuma ligação aberta ainda.`;
    return;
  }
  for (const e of estado) {
    const linha = elemento('div', 'diag__linha');
    linha.append(elemento('span', 'diag__quem', nomeDoPeer(e.peer)));
    const caminho = e.relay ? 'por relay' : 'direta';
    const ms = e.ms ? ` · ${Math.round(e.ms)} ms` : '';
    const voz_ = `voz ↑${e.enviados} ↓${e.recebidos}`;
    const d = elemento('span', e.recebidos === 0 && e.enviados > 0 ? 'diag__mudo' : null,
      `${caminho}${ms} · ${voz_}`);
    linha.append(d);
    alvo.append(linha);
  }
}
$('#fechar-rede').onclick = () => fechar('veu-rede');

async function entrarEmVoz(servidor, canal) {
  vozFalhou = null;
  vozPartida.clear();
  if (voz.canal === canal) return;
  await sairDeVoz(false);
  voz.servidor = servidor;
  voz.canal = canal;
  // Desenhar JA, antes de pedir o microfone. O pedido de autorizacao pode ficar minutos
  // a espera de resposta -- ou nunca ser respondido -- e ate la a app parecia presa a
  // dizer "nao estas nesta sala" quando ja estava.
  desenharVoz();
  desenharCanais();
  await invoke('presenca_de_voz', { servidor, canal }).catch(console.error);

  try {
    // Com limite de tempo: se ninguem responder ao pedido, entra-se sem microfone em vez
    // de ficar pendurado para sempre.
    voz.micro = await Promise.race([
      navigator.mediaDevices.getUserMedia({
        audio: { echoCancellation: true, noiseSuppression: true },
      }),
      new Promise((_, rej) => setTimeout(() => rej(new Error('sem resposta ao pedido')), 20000)),
    ]);
    if (voz.canal !== canal) {          // saiu enquanto se esperava
      voz.micro.getTracks().forEach(t => t.stop());
      voz.micro = null;
      return;
    }
    comecarAEnviarVoz(voz.micro);
    vigiarAudio(voz.eu, voz.micro);
    desenharVoz();
  } catch (e) {
    // Sem microfone continua a dar para ouvir e para partilhar ecra.
    console.warn('sem microfone:', e);
    // Aparecer mudo SEM RAZÃO é o pior dos dois mundos: a pessoa vê o ícone cortado e
    // assume que carregou nele sem querer.
    vozFalhou = `Sem microfone: ${e && e.message ? e.message : e}`;
    voz.micro = null;
  }
  desenharVoz();
}

async function sairDeVoz(anunciar = true) {
  // A câmara desliga-se ao sair, sempre. Deixar a luz acesa depois de sair da chamada era
  // a pior falha de confiança que esta app podia ter.
  pararDeEnviarCamara();
  for (const chave of [...camarasRecebidas.keys()]) fecharCamaraRecebida(chave);
  if (anunciar && voz.canal) {
    await invoke('presenca_de_voz', { servidor: voz.servidor, canal: null }).catch(() => {});
  }
  pararDeEnviarVoz();
  for (const chave of [...voz.audio.keys()]) calarPeer(chave);
  if (voz.micro) voz.micro.getTracks().forEach(t => t.stop());
  if (voz.ecra) invoke('parar_de_partilhar').catch(() => {});
  if (voz.vejoMeuEcra) { voz.vejoMeuEcra.fechar(); voz.vejoMeuEcra = null; }
  for (const chave of [...fluxosRecebidos.keys()]) fecharFluxoRecebido(chave);
  for (const chave of [...voz.analisadores.keys()]) pararDeVigiar(chave);
  voz.falando.clear();
  voz.aPartilhar.clear();
  voz.comCamara.clear();
  voz.infoDaTransmissao.clear();
  voz.entendeCamara.clear();
  voz.entendeSom.clear();
  voz.jaFalou.clear();
  voz.aVer = null;
  voz.micro = null; voz.ecra = null; voz.ecraTamanho = null; voz.qualidadeEmUso = null; voz.qualidadeEmUso = null;
  voz.canal = null;
  desenharVoz();
  desenharRodape();
}

function sinalizar(peer, dados) {
  invoke('enviar_sinal', {
    para: peer, servidor: voz.servidor, canal: voz.canal, dados: JSON.stringify(dados),
  }).catch(console.error);
}

/** Os avisos que trocamos entre nós: quem está a transmitir e quem está a ver.
 *
 *  Já não há aqui SDP nem candidatos ICE. A voz e o ecrã vão os dois pelo iroh, que trata
 *  do NAT sozinho — o que desapareceu com o WebRTC foi a negociação toda e, com ela, a
 *  necessidade de configurar servidores de ligação à mão.
 */
async function receberSinal(de, dados) {
  if (dados.tipo === 'assistir') {
    if (dados.ligado) voz.aSerVistoPor.add(de); else voz.aSerVistoPor.delete(de);
    actualizarEspectadores();
    return;
  }
  if (dados.tipo === 'estado') {
    const versao = Number.isFinite(dados.v) ? dados.v : 1;
    voz.jaFalou.add(de);
    if (versao >= 2) {
      voz.entendeCamara.add(de);
      voz.entendeSom.add(de);
    } else {
      voz.entendeCamara.delete(de);
      voz.entendeSom.delete(de);
    }
    if (dados.ecra) {
      voz.aPartilhar.add(de);
    } else {
      voz.aPartilhar.delete(de);
      fecharFluxoRecebido(de);
    }
    if (dados.ecra) {
      voz.infoDaTransmissao.set(de, {
        qualidade: typeof dados.qualidade === 'string' ? dados.qualidade : null,
        espectadores: Number.isFinite(dados.espectadores) ? dados.espectadores : 0,
      });
    } else {
      voz.infoDaTransmissao.delete(de);
    }
    if (dados.camara) {
      voz.comCamara.add(de);
    } else {
      voz.comCamara.delete(de);
      fecharCamaraRecebida(de);
    }
    if (voz.aVer === de && !dados.ecra) voz.aVer = null;
    desenharVoz();
  }
}

/* ==========================================================================
   Partilha de ecrã: captada e codificada em Rust, não pela webview.

   O `getDisplayMedia` funcionava, mas trazia duas coisas que não se resolvem por
   configuração: o WebView2 desenhava por cima da app a barra "está a partilhar uma
   janela" — não há API nem flag para a tirar, porque é o indicador de segurança dele — e
   o codificador acabava por ser software, com a placa parada ao lado.

   Agora o Rust capta, codifica com o codificador da placa, e manda pedaços de MP4
   fragmentado. Aqui só se juntam os pedaços e se entregam a um `<video>` pelo
   MediaSource, que é o que o navegador sabe fazer sem ajuda.
   ========================================================================== */

/* As etiquetas com que o Rust marca cada pedaço: bytes de vídeo, ou o nome do codec. */
const ETIQUETA_BYTES = 0;
const ETIQUETA_CODEC = 1;

/** Um `<video>` alimentado aos pedaços.
 *
 *  O MediaSource não aceita bytes enquanto está ocupado a digerir os anteriores, e o
 *  `appendBuffer` atira exceção se lho fizerem. Por isso há fila: os pedaços chegam ao
 *  ritmo do codificador, não ao ritmo a que o navegador os quer.
 */
function fluxoDePedacos() {
  const media = new MediaSource();
  const el = document.createElement('video');
  el.autoplay = true;
  el.playsInline = true;
  el.muted = true;
  el.src = URL.createObjectURL(media);

  const fila = [];
  let buffer = null;
  let codec = null;
  let aberto = false;
  // Porque é que este fluxo não dá imagem, se não der. O comentário aqui ao lado já avisava
  // que o codec VARIA com a placa gráfica de quem partilha — ou seja, isto vai acontecer em
  // máquinas reais, e ia parecer um problema de ligação.
  let recusa = null;

  /* O codec não se assume, vem escrito no fluxo.
     O `addSourceBuffer` obriga a declará-lo, e o navegador VALIDA o que se lhe declara
     contra o cabeçalho: se não bater certo, recusa tudo com um "stream parsing failed"
     que não explica nada. Nesta máquina a NVIDIA produz Baseline 4.2; noutra placa será
     outro. Por isso espera-se por ele antes de abrir o buffer. */
  const abrir = () => {
    if (buffer || !aberto || !codec) return;
    const tipo = `video/mp4; codecs="${codec}"`;
    el.__codec = codec;
    if (!window.MediaSource || !MediaSource.isTypeSupported(tipo)) {
      console.warn('esta webview não sabe descodificar', tipo);
      recusa = `Esta máquina não sabe descodificar ${tipo}. `
        + 'A placa gráfica de quem partilha produziu um formato que esta não lê.';
      desenharVoz();
      return;
    }
    try {
      buffer = media.addSourceBuffer(tipo);
      buffer.mode = 'sequence';
      buffer.addEventListener('updateend', escoar);
      escoar();
    } catch (e) {
      console.warn('não consegui abrir o buffer de vídeo:', e);
      recusa = `Não consegui abrir o vídeo: ${e && e.message ? e.message : e}`;
      desenharVoz();
    }
  };

  const escoar = () => {
    if (!buffer || buffer.updating || !fila.length) return;
    try {
      buffer.appendBuffer(fila.shift());
    } catch (e) {
      // QuotaExceeded: o buffer encheu. Deita-se fora o que já passou — numa transmissão
      // ao vivo ninguém quer rebobinar, e guardar tudo acabaria por rebentar a memória.
      if (e.name === 'QuotaExceededError' && el.buffered.length) {
        try { buffer.remove(0, Math.max(0, el.currentTime - 2)); } catch (_) { /* logo se vê */ }
      } else {
        console.warn('o vídeo recusou o pedaço:', e);
      }
    }
  };

  /** Mantém quem vê colado ao PRESENTE.
   *
   *  # A avaria que isto corrige
   *
   *  Um `<video>` reproduz a 1× e não sabe que isto é ao vivo. Quem carrega em "Assistir"
   *  entra pelo princípio do que já foi enviado, e a partir daí só perde terreno: chega
   *  mais um segundo de imagem por cada segundo que passa, e o atraso NUNCA encolhe. Foi
   *  medido: buffer com 25 segundos e o leitor em 6,62 — dezoito segundos atrás. Quem
   *  partilha mexe o rato e o outro só vê isso vinte segundos depois; do lado de lá parece
   *  que a partilha não está a funcionar, e no fundo está — só que no passado.
   *
   *  Nota-se mais num ecrã largo, porque gera mais dados e o descodificador perde terreno
   *  mais depressa. Foi por isso que apareceu ao partilhar o ultrawide e não o 16:9.
   *
   *  A cura é a de qualquer leitor ao vivo: quando o atraso passa de um limite, SALTA-SE
   *  para a frente. Salta-se em vez de acelerar porque um salto é honesto — perde-se o que
   *  se perdeu — e acelerar a imagem e o som soa mal e nunca chega a apanhar.
   */
  const FOLGA = 0.6;      // o quanto se fica atrás da ponta, para não secar a cada soluço
  const LIMITE = 2.0;     // acima disto salta-se; abaixo, deixa-se estar
  const perseguirOVivo = () => {
    if (!buffer || !el.buffered.length) return;
    const ponta = el.buffered.end(el.buffered.length - 1);
    const atraso = ponta - el.currentTime;
    if (atraso > LIMITE) {
      const destino = Math.max(0, ponta - FOLGA);
      // `fastSeek` não existe em todo o lado e é aproximado; a atribuição direta é exata.
      try { el.currentTime = destino; } catch (e) { /* o próximo tick tenta */ }
    }
    // E deita-se fora o passado, que ninguém vai rebobinar. Sem isto a memória do buffer
    // cresce durante toda a chamada, e é o mesmo que a levar a rebentar devagar.
    if (!buffer.updating && el.buffered.length) {
      const inicio = el.buffered.start(0);
      const corte = el.currentTime - 4;
      if (corte > inicio + 2) {
        try { buffer.remove(inicio, corte); } catch (e) { /* logo se vê */ }
      }
    }
    // Um `<video>` que ficou sem dados a meio fica em pausa e não volta sozinho.
    //
    // Mas uma pausa PEDIDA é outra coisa. Quem partilha e muda de janela tem a
    // pré-visualização pausada de propósito, para poupar recursos — e sem esta bandeira
    // este relógio desfazia-a meio segundo depois. Duas correções minhas a lutar uma com a
    // outra, e a que corre mais vezes ganhava.
    if (el.paused && el.readyState >= 2 && !el.__pausaPedida) el.play().catch(() => {});
  };
  const relogioDoVivo = setInterval(perseguirOVivo, 500);

  media.addEventListener('sourceopen', () => {
    aberto = true;
    abrir();
  }, { once: true });

  return {
    el,
    empurrar(marcado) {
      if (!marcado.length) return;
      const etiqueta = marcado[0];
      const bytes = marcado.subarray(1);
      if (etiqueta === ETIQUETA_CODEC) {
        codec = new TextDecoder().decode(bytes);
        abrir();
        return;
      }
      if (etiqueta !== ETIQUETA_BYTES) return;
      fila.push(bytes);
      // Se a fila crescer é porque o navegador não acompanha; nesse caso o que interessa
      // é o presente, não o passado.
      if (fila.length > 60) fila.splice(0, fila.length - 30);
      escoar();
    },
    /** A razão de não haver imagem, quando não há. `null` significa "ainda a chegar". */
    porqueNaoDa() { return recusa; },
    fechar() {
      clearInterval(relogioDoVivo);
      try { el.pause(); } catch (e) { /* já parado */ }
      try { URL.revokeObjectURL(el.src); } catch (e) { /* já libertado */ }
      el.removeAttribute('src');
      fila.length = 0;
    },
  };
}

/** Abre o seletor: um separador por tipo de fonte, como no Discord —
 *  Janelas num, Ecrãs noutro, cada um só com o seu. */
let fontesEmMemoria = [];
let abaActiva = 'janela';

function desenharFontes() {
  const lista = $('#lista-fontes');
  document.querySelectorAll('#abas-fontes .aba').forEach(a => {
    a.classList.toggle('is-activa', a.dataset.aba === abaActiva);
  });
  lista.textContent = '';
  const visiveis = fontesEmMemoria.filter(f => f.tipo === abaActiva);
  if (!visiveis.length) {
    lista.append(elemento('p', 'fontes__espera',
      abaActiva === 'ecra' ? 'não encontrei ecrãs' : 'não encontrei janelas para partilhar'));
    return;
  }
  for (const f of visiveis) {
    const cartao = elemento('button', 'fonte');
    if (f.miniatura) {
      const img = document.createElement('img');
      img.src = f.miniatura;
      cartao.append(img);
    }
    const nome = elemento('span', null, f.titulo);
    nome.title = f.titulo;
    cartao.append(nome);
    cartao.onclick = () => { fechar('veu-fontes'); iniciarPartilha(f.id); };
    lista.append(cartao);
  }
}

/** Quem, na sala, ainda não percebe o que esta versão envia.
 *
 *  Enquanto o `estado` do outro lado não chegar, ele não está em `entendeSom` — e não se
 *  avisa por isso: seria acusar toda a gente de estar desactualizada durante o primeiro
 *  segundo de cada chamada. Só conta quem já falou e disse ser antigo.
 */
function quemNaoVaiVer() {
  return [...voz.presentes.entries()]
    .filter(([p, c]) => c === voz.canal && voz.jaFalou.has(p) && !voz.entendeSom.has(p))
    .map(([p]) => p);
}

function desenharAvisoDeVersao() {
  const el = $('#aviso-versao');
  if (!el) return;
  const antigos = quemNaoVaiVer();
  const comSom = qualidadeDePartilha().som;
  if (!antigos.length) { el.hidden = true; return; }
  const nomes = antigos.map(nomeDoPeer).join(', ');
  el.hidden = false;
  // Duas mensagens diferentes porque as consequências são diferentes, e dizer "pode haver
  // problemas" às duas seria não dizer nada.
  el.textContent = comSom
    ? `${nomes} está numa versão antiga e NÃO vai ver esta partilha, porque ela leva o som `
      + `do sistema — a versão dele só sabe ler a imagem. Desliga o som na engrenagem, ou `
      + `pede-lhe para actualizar (a app faz isso sozinha ao reabrir).`
    : `${nomes} está numa versão antiga. A imagem vai ver; a câmara, não.`;
}

async function escolherFonte() {
  abrir('veu-fontes');
  desenharAvisoDeVersao();
  const lista = $('#lista-fontes');
  lista.innerHTML = '<p class="fontes__espera">a olhar para as janelas…</p>';
  fontesEmMemoria = await invoke('fontes_de_partilha').catch(() => []);
  if ($('#veu-fontes').hidden) return;   // cancelou antes de a lista chegar
  desenharFontes();
}

document.querySelectorAll('#abas-fontes .aba').forEach(a => {
  a.onclick = () => { abaActiva = a.dataset.aba; desenharFontes(); };
});
$('#fechar-fontes').onclick = () => fechar('veu-fontes');

/* --- o modo de transmissão, no menu da engrenagem -------------------------- */

/** A escolha guardada. Um MODO é um atalho: "jogos" e "texto" trazem os números feitos;
 *  "pers" usa os que a pessoa afinou. Mexer num número à mão muda para "pers" sozinho,
 *  como no Discord — escolher um valor É personalizar. */
const MODOS = {
  jogos: { altura: 1440, fps: 60 },
  texto: { altura: 0, fps: 15 },
};

function qualidadeDePartilha() {
  let q = {};
  try {
    q = JSON.parse(localStorage.getItem('bruma.qualidade') || '{}');
  } catch (e) { /* estraga-se, recomeça-se */ }
  return {
    modo: ['jogos', 'texto', 'pers'].includes(q.modo) ? q.modo : 'pers',
    altura: Number.isFinite(q.altura) ? q.altura : 1080,
    fps: [15, 30, 60].includes(q.fps) ? q.fps : 60,
    debito: Number.isFinite(q.debito) ? q.debito : 0,
    // Liga por omissão, como no Discord: quem partilha um jogo ou um vídeo quer que se
    // ouça, e quem não quer diz aqui — em vez de descobrir a meio que ninguém ouviu nada.
    som: q.som !== false,
  };
}

/** Os números que valem MESMO, resolvido o modo. O débito manual vale em qualquer modo. */
function qualidadeEfetiva() {
  const q = qualidadeDePartilha();
  const base = MODOS[q.modo] || { altura: q.altura, fps: q.fps };
  return { altura: base.altura, fps: base.fps, debito: q.debito, som: q.som };
}

$('#linha-som').onclick = () => guardarQualidade({ som: !qualidadeDePartilha().som });

function guardarQualidade(mudanca) {
  const q = { ...qualidadeDePartilha(), ...mudanca };
  localStorage.setItem('bruma.qualidade', JSON.stringify(q));
  desenharMenuTransmissao();
}

const OPCOES_ALTURA = [[720, '720p'], [1080, '1080p'], [1440, '1440p'], [0, 'Nativa']];
const OPCOES_FPS = [[15, '15'], [30, '30'], [60, '60']];
const OPCOES_DEBITO = [[0, 'automático'], [3_000_000, '3 Mbps'], [6_000_000, '6 Mbps'],
                       [10_000_000, '10 Mbps'], [16_000_000, '16 Mbps']];

function rotuloDe(opcoes, valor) {
  const par = opcoes.find(([v]) => v === valor);
  return par ? par[1] : String(valor);
}

function desenharMenuTransmissao() {
  const q = qualidadeDePartilha();
  const efetiva = qualidadeEfetiva();

  document.querySelectorAll('[data-modo]').forEach(l => {
    l.classList.toggle('is-activa', l.dataset.modo === q.modo);
  });
  $('#desc-pers').textContent =
    `${q.altura === 0 ? 'nativa' : q.altura + 'p'}, ${q.fps} ips`;
  $('#valor-altura').textContent = rotuloDe(OPCOES_ALTURA, efetiva.altura);
  $('#valor-fps').textContent = rotuloDe(OPCOES_FPS, efetiva.fps);
  $('#valor-debito').textContent = rotuloDe(OPCOES_DEBITO, q.debito);
  // A caixa diz SILENCIAR: marcada é som desligado. É a mesma leitura do Discord, e é o
  // contrário do que a variável guarda — daí a negação estar aqui, uma vez só.
  $('#mudo-transmissao').checked = !q.som;
  $('#desc-som').textContent = q.som
    ? 'o som das colunas segue com a imagem'
    : 'a transmissão vai muda';
  desenharAvisoDeVersao();

  const nome = q.modo === 'jogos' ? 'Jogos' : q.modo === 'texto' ? 'Texto' : null;
  $('#resumo-qualidade').textContent =
    `${nome ? nome + ' · ' : ''}${efetiva.altura === 0 ? 'Nativa' : efetiva.altura + 'p'}`
    + ` · ${efetiva.fps} ips${q.debito ? ' · ' + rotuloDe(OPCOES_DEBITO, q.debito) : ''}`;

  const subs = [
    ['#sub-altura', OPCOES_ALTURA, q.altura, v => guardarQualidade({ altura: v, modo: 'pers' })],
    ['#sub-fps', OPCOES_FPS, q.fps, v => guardarQualidade({ fps: v, modo: 'pers' })],
    ['#sub-debito', OPCOES_DEBITO, q.debito, v => guardarQualidade({ debito: v })],
  ];
  for (const [sel, opcoes, atual, aoEscolher] of subs) {
    const sub = $(sel);
    if (sub.hidden) continue;
    sub.textContent = '';
    for (const [valor, rotulo] of opcoes) {
      const b = elemento('button', 'menu-trans__opcao', rotulo);
      if (valor === atual) b.classList.add('is-activa');
      // Escolher fecha o submenu, como no Discord: a lista aberta depois da decisão é
      // só ruído entre a pessoa e o resto do menu.
      b.onclick = () => { sub.hidden = true; aoEscolher(valor); };
      sub.append(b);
    }
  }
}

$('#btn-qualidade').onclick = ev => {
  ev.stopPropagation();
  const m = $('#menu-transmissao');
  m.hidden = !m.hidden;
  $('#btn-qualidade').classList.toggle('is-on', !m.hidden);
  if (m.hidden) document.querySelectorAll('.menu-trans__sub').forEach(s => { s.hidden = true; });
  desenharMenuTransmissao();
};

document.querySelectorAll('[data-modo]').forEach(l => {
  l.onclick = () => guardarQualidade({ modo: l.dataset.modo });
});
document.querySelectorAll('[data-abre]').forEach(l => {
  l.onclick = () => {
    const sub = $('#sub-' + l.dataset.abre);
    sub.hidden = !sub.hidden;
    desenharMenuTransmissao();
  };
});
// Clicar fora fecha o menu, como qualquer menu decente.
document.addEventListener('mousedown', ev => {
  const m = $('#menu-transmissao');
  if (m.hidden) return;
  if (ev.target.closest('#menu-transmissao, #btn-qualidade')) return;
  m.hidden = true;
  $('#btn-qualidade').classList.remove('is-on');
  document.querySelectorAll('.menu-trans__sub').forEach(s => { s.hidden = true; });
});
desenharMenuTransmissao();

async function alternarEcra() {
  if (voz.ecra) {
    await invoke('parar_de_partilhar').catch(() => {});
    if (voz.vejoMeuEcra) { voz.vejoMeuEcra.fechar(); voz.vejoMeuEcra = null; }
    if (voz.aVer === voz.eu) voz.aVer = null;
    voz.ecra = null; voz.ecraTamanho = null; voz.qualidadeEmUso = null;
    anunciarEstado();
    desenharVoz();
    desenharRodape();
    return;
  }
  if (!voz.canal || !voz.servidor) return;
  escolherFonte();
}

async function iniciarPartilha(fonte) {
  if (voz.ecra || !voz.canal || !voz.servidor) return;
  partilhaFalhou = null;
  partilhaAviso = null;

  // O canal fica aberto desde já, mas o Rust só manda por ele quando estivermos mesmo
  // a olhar (ver_meu_ecra). Criar o <video> agora e deixá-lo a apanhar pedaços às
  // escuras era o bug do ecrã preto: a fila aparava os antigos e o cabeçalho ia fora.
  const canal = new window.__TAURI__.core.Channel();
  canal.onmessage = pedaco => {
    if (voz.vejoMeuEcra) voz.vejoMeuEcra.empurrar(
      pedaco instanceof ArrayBuffer ? new Uint8Array(pedaco) : new Uint8Array(pedaco));
  };

  const q = qualidadeEfetiva();
  let medido = null;
  try {
    medido = await invoke('comecar_a_partilhar', {
      servidor: voz.servidor,
      canalVoz: voz.canal,
      fonte,
      altura: q.altura,
      fps: q.fps,
      debito: q.debito,
      comSom: q.som,
      saida: canal,
    });
  } catch (e) {
    console.warn('não consegui começar a partilhar:', e);
    return;
  }
  // O tamanho REAL com que a captura ficou. Com "Nativa" é a única forma de dizer a quem
  // assiste que resolução está a receber — só o Rust o soube calcular.
  voz.ecraTamanho = medido && Number.isFinite(medido.altura) ? medido : null;
  voz.qualidadeEmUso = { altura: q.altura, fps: q.fps, debito: q.debito, som: q.som };
  voz.ecra = { fechar() {} };
  // O tamanho REAL com que a captura ficou. Com "Nativa" é a única forma de dizer a quem
  // assiste que resolução está a receber, porque só o Rust o soube calcular.
  try {
    const r = await invoke('capacidades', { linha: '' }).catch(() => null);
    void r;
  } catch (e) { /* não faz mal */ }
  anunciarEstado();
  desenharVoz();
  desenharRodape();
}

/* --- receber o ecrã dos outros -------------------------------------------- */

/* Um canal só, e é o cabeçalho de cada pedaço que diz de quem ele é. O Rust põe à frente
   o tamanho da chave e a chave; o resto são os bytes do vídeo. */
const fluxosRecebidos = new Map();

(function ligarEntradaDeEcra() {
  if (!window.__TAURI__) return;
  const canal = new window.__TAURI__.core.Channel();
  canal.onmessage = pedaco => {
    const bytes = pedaco instanceof ArrayBuffer ? new Uint8Array(pedaco) : new Uint8Array(pedaco);
    if (!bytes.length) return;
    const n = bytes[0];
    if (bytes.length < 1 + n) return;
    const chave = new TextDecoder().decode(bytes.subarray(1, 1 + n));
    const corpo = bytes.subarray(1 + n);
    let fluxo = fluxosRecebidos.get(chave);
    if (!fluxo) {
      fluxo = fluxoDePedacos();
      fluxosRecebidos.set(chave, fluxo);
      // O painel só sabe que há imagem depois do primeiro pedaço.
      desenharVoz();
    }
    fluxo.empurrar(corpo);
  };
  invoke('receber_ecra', { canal }).catch(() => {});
})();

/** Diz ao Rust quem está mesmo a ver. Enquanto isto estiver vazio, nada sai da máquina. */
function actualizarEspectadores() {
  if (!voz.ecra) return;
  // Neste momento quem assiste é quem tem a transmissão aberta; a interface ainda não
  // distingue "aberto mas minimizado", e é aí que vive a próxima poupança de upload.
  const lista = [...voz.aSerVistoPor].filter(p => voz.presentes.get(p) === voz.canal);
  invoke('definir_espectadores', { chaves: lista }).catch(() => {});
}

function nomeDoPeer(peer) {
  if (peer === voz.eu) return 'tu';
  const s = servidor();
  const m = s && s.membros.find(x => x.chave === peer);
  return m ? m.nome : `${peer.slice(0, 6)}…`;
}

/** Um painel da grelha da chamada.
 *
 *  Três estados possíveis, e são mesmo diferentes:
 *   - a transmitir: a foto sai da frente e fica o convite para assistir;
 *   - com vídeo a ser visto: o vídeo ocupa tudo;
 *   - sem vídeo: a foto, com anel verde quando a pessoa fala.
 */
/** O `<video>` da transmissão de ecrã, se houver.
 *
 *  O ecrã já não é um MediaStream: vem em pedaços de MP4 e vive num `<video>` próprio,
 *  criado uma vez e reaproveitado. Redesenhar o painel não pode criar um novo, senão
 *  perdia-se tudo o que já foi recebido a cada mudança de ecrã.
 */
function ecraDe(chave) {
  if (chave === voz.eu) return voz.vejoMeuEcra ? voz.vejoMeuEcra.el : null;
  const f = fluxosRecebidos.get(chave);
  return f ? f.el : null;
}

/** Alguém está mesmo nesta sala? É o que resta de "há ligação a esta pessoa" agora que
 *  não há PeerConnections para consultar. */
function estaNaSala(chave) {
  return chave === voz.eu || voz.presentes.get(chave) === voz.canal;
}

function fecharFluxoRecebido(chave) {
  const f = fluxosRecebidos.get(chave);
  if (!f) return;
  f.fechar();
  fluxosRecebidos.delete(chave);
}

function painelDeVoz(chave, opcoes = {}) {
  const t = elemento('div', 'tile');
  t.dataset.chave = chave;
  if (voz.falando.has(chave)) t.classList.add('a-falar');

  const transmite = voz.aPartilhar.has(chave) || (chave === voz.eu && !!voz.ecra);
  const aVer = opcoes.aVer;
  const temVideo = voz.comCamara.has(chave) || (chave === voz.eu && !!voz.camara);

  if (transmite && !aVer) {
    // Enquanto não se carrega em Assistir, não se descodifica nada: poupa CPU de quem
    // está na sala só para ouvir, que é a maioria das vezes.
    const bloco = elemento('div', 'tile__transmite');
    const marca = elemento('span', 'ident');
    pintar(marca, chave);
    bloco.append(marca);
    bloco.append(elemento('b', null, `${nomeDoPeer(chave)} está a transmitir`));
    const b = elemento('button', 'btn btn--primary',
      chave === voz.eu ? 'Ver o que estás a enviar' : 'Assistir');
    b.onclick = () => assistir(chave);
    bloco.append(b);
    if (chave === voz.eu) {
      bloco.append(elemento('span', 'tile__dica', 'é o teu ecrã, tal como sai daqui'));
    }
    t.append(bloco);
  } else if (transmite && aVer) {
    // O <video> do ecrã é reaproveitado, nunca recriado: ele já tem dentro tudo o que
    // chegou até agora, e criar outro aqui deitava isso fora a cada redesenho.
    const el = ecraDe(chave);
    if (el) t.append(el);
    else t.append(elemento('div', 'tile__sem-video', 'à espera da imagem…'));
  } else if (temVideo) {
    // A câmara ganha ao avatar mas perde para a transmissão: quem está a mostrar o ecrã
    // está a mostrá-lo por alguma razão, e a cara cabe no painel de quem não está.
    const el = chave === voz.eu ? meuEspelho() : camaraDe(chave).tela;
    if (el) t.append(el);
  } else {
    const sem = elemento('div', 'tile__sem-video');
    const av = elemento('span', 'ident');
    pintar(av, chave);
    sem.append(av);
    // Só se escreve alguma coisa quando ainda NÃO há ligação. "Só áudio" seria
    // ruído: a foto sozinha já diz que não há vídeo.
    if (!estaNaSala(chave)) sem.append(elemento('span', null, 'a ligar…'));
    t.append(sem);
  }

  const partido = vozPartida.get(chave);
  if (partido) {
    // Sem isto, quem deixa de ouvir uma pessoa suspeita DELA — e ela continua ali, a
    // aparecer presente e sem noção de nada.
    const aviso = elemento('span', 'tile__sem-audio', 'sem áudio');
    aviso.title = partido;
    t.append(aviso);
  }
  t.append(elemento('span', 'tile__nome', nomeDoPeer(chave)));
  t.append(accoesDoPainel(chave, { transmite, aVer, temVideo }));
  return t;
}

/** Quantos, ao todo, estão a ver a transmissão de `quem` — incluindo eu. */
function espectadoresDe(quem) {
  if (quem === voz.eu) return voz.aSerVistoPor.size + (voz.aVer === voz.eu ? 1 : 0);
  const i = voz.infoDaTransmissao.get(quem);
  return i ? i.espectadores : 0;
}

function qualidadeDe(quem) {
  if (quem === voz.eu) return rotuloDaQualidade();
  const i = voz.infoDaTransmissao.get(quem);
  return i && i.qualidade ? i.qualidade : null;
}

/** Se as fotinhas por baixo do palco estão escondidas. */
let gentePorBaixoOculta = false;

/** Se a janela do Bruma tem o foco — ver a pausa da pré-visualização no palco. */
let janelaComFoco = document.hasFocus();
window.addEventListener('focus', () => { janelaComFoco = true; if (voz.aVer) desenharVoz(); });
window.addEventListener('blur', () => { janelaComFoco = false; if (voz.aVer) desenharVoz(); });

const ICO = {
  mic: '<path d="M10 3.2a2.1 2.1 0 0 1 2.1 2.1v4.4a2.1 2.1 0 0 1-4.2 0V5.3A2.1 2.1 0 0 1 10 3.2Z"/><path d="M5.4 9.3a4.6 4.6 0 0 0 9.2 0M10 13.9v2.9"/>',
  camara: '<rect x="2.6" y="5.4" width="10" height="9.2" rx="2"/><path d="M12.6 9.2l4.8-2.6v6.8l-4.8-2.6Z"/>',
  gente: '<circle cx="7.6" cy="7.4" r="2.6"/><path d="M3 16.2c0-2.5 2.1-4 4.6-4s4.6 1.5 4.6 4"/><path d="M13.4 5.2a2.4 2.4 0 0 1 0 4.5M14.4 12.6c1.7.4 2.9 1.6 2.9 3.6"/>',
  parar: '<rect x="4.6" y="4.6" width="10.8" height="10.8" rx="2.2"/>',
  desligar: '<path d="M4.2 8.4c3.4-2.2 8.2-2.2 11.6 0l.9 2.3-3 .5-.6-1.9c-1.9-.8-4.3-.8-6.2 0l-.6 1.9-3-.5Z"/>',
  convidar: '<circle cx="8" cy="7.2" r="2.7"/><path d="M3.3 16c0-2.6 2.2-4.2 4.7-4.2 1 0 1.9.2 2.7.7"/><path d="M14.6 11.4v4.8M12.2 13.8h4.8"/>',
  olho: '<circle cx="6" cy="5.6" r="2.1"/><path d="M2 13c0-2.1 1.8-3.4 4-3.4s4 1.3 4 3.4"/><path d="M11 4.4a2 2 0 0 1 0 3.8M12 10.2c1.4.3 2.4 1.3 2.4 2.9"/>',
};

/** Um botão redondo da barra flutuante. */
function botaoDoPalco(desenho, titulo, aoClicar, opcoes) {
  opcoes = opcoes || {};
  const b = elemento('button', 'palco__bt' + (opcoes.classe ? ' ' + opcoes.classe : ''));
  b.title = titulo;
  b.setAttribute('aria-label', titulo);
  b.innerHTML = '<svg viewBox="0 0 20 20" width="17" height="17">' + desenho + '</svg>';
  if (opcoes.cortado) b.classList.add('is-cortado');
  b.onclick = aoClicar;
  return b;
}

/** O palco: a transmissão a ocupar tudo, com as camadas por cima e as fotinhas por baixo.
 *
 *  # Porque é que tudo flutua
 *
 *  Uma transmissão de 3440x1440 numa janela já é pequena; roubar-lhe barras fixas em cima
 *  e em baixo custava mais do que aquilo que essas barras dizem. Aparecem com o rato por
 *  cima e saem sozinhas, como em qualquer leitor a sério — e o `:focus-within` garante que
 *  quem anda pelo teclado também lhes chega.
 */
function palcoDeTransmissao(quem, canal, outros) {
  const souEu = quem === voz.eu;
  const palco = elemento('div', 'palco');
  const vidro = elemento('div', 'palco__vidro');

  const el = ecraDe(quem);
  const fluxo = quem === voz.eu ? voz.vejoMeuEcra : fluxosRecebidos.get(quem);
  const recusa = fluxo && fluxo.porqueNaoDa && fluxo.porqueNaoDa();
  if (recusa) {
    // Um rectângulo preto para sempre é indistinguível de "a rede está má". Dizer a razão
    // é a diferença entre a pessoa esperar em vão e saber que não vale a pena.
    const caixa = elemento('div', 'palco__pausa');
    caixa.append(elemento('b', null, 'Não consigo mostrar esta transmissão'));
    caixa.append(elemento('span', null, recusa));
    vidro.append(caixa);
  } else if (el) {
    vidro.append(el);
  } else {
    vidro.append(elemento('div', 'tile__sem-video', 'à espera da imagem…'));
  }

  // --- cima à esquerda: onde estou e de quem é isto
  const onde = elemento('div', 'palco__onde');
  onde.append(elemento('b', null, canal ? canal.nome : 'Sala'));
  onde.append(elemento('i', null, souEu ? 'a tua transmissão' : nomeDoPeer(quem)));
  const camadaEsq = elemento('div', 'palco__camada palco__cima-esq');
  // A saída. Não estava no pedido, mas sem ela fica-se preso no palco: as fotinhas de
  // baixo levam a OUTRAS transmissões, nenhuma leva de volta à sala.
  camadaEsq.append(botaoDoPalco(
    '<path d="M12 4.5 6.5 10l5.5 5.5"/>', 'Voltar à sala', pararDeAssistir));
  camadaEsq.append(onde);
  vidro.append(camadaEsq);

  // --- cima à direita: qualidade, quantos veem, e o AO VIVO
  const camadaDir = elemento('div', 'palco__camada palco__cima-dir');
  const qual = qualidadeDe(quem);
  if (qual) {
    const selo = elemento('span', 'palco__selo', qual);
    // A explicação vive AQUI e não numa nota à parte: é neste número que a pessoa repara
    // quando ele não é o que ela escolheu, e é aqui que a pergunta nasce.
    const porque = souEu ? porqueEstaResolucao() : null;
    selo.title = porque || 'a resolução e o ritmo desta transmissão';
    if (porque && voz.qualidadeEmUso && voz.ecraTamanho
        && voz.qualidadeEmUso.altura && voz.ecraTamanho.altura < voz.qualidadeEmUso.altura) {
      selo.classList.add('palco__selo--nota');
    }
    camadaDir.append(selo);
  }
  const olhos = elemento('span', 'palco__selo');
  olhos.innerHTML =
    '<svg viewBox="0 0 16 16" width="12" height="12" fill="none" stroke="currentColor"'
    + ' stroke-width="1.4">' + ICO.olho + '</svg>';
  olhos.append(elemento('span', null, String(espectadoresDe(quem))));
  camadaDir.append(olhos);
  const vivo = elemento('span', 'palco__selo palco__selo--vivo');
  vivo.innerHTML = '<i class="ponto"></i>';
  vivo.append(document.createTextNode('AO VIVO'));
  camadaDir.append(vivo);
  vidro.append(camadaDir);

  // --- baixo à esquerda: convidar alguém a ver
  const camadaConv = elemento('div', 'palco__camada palco__camada--baixo palco__baixo-esq');
  camadaConv.append(botaoDoPalco(ICO.convidar, 'Convidar para assistir', () => {
    // O convite é o do servidor: quem entra por ele chega à sala e vê o que está a dar.
    $('#btn-convite').click();
  }));
  vidro.append(camadaConv);

  // --- baixo ao meio: os controlos
  const barra = elemento('div', 'palco__camada palco__camada--baixo palco__meio');
  const faixaMic = voz.micro ? voz.micro.getAudioTracks()[0] : null;
  barra.append(botaoDoPalco(
    ICO.mic,
    !faixaMic ? 'Sem microfone' : (faixaMic.enabled ? 'Silenciar microfone' : 'Ligar microfone'),
    () => $('#btn-mic').click(),
    { cortado: !!faixaMic && !faixaMic.enabled },
  ));
  barra.append(botaoDoPalco(
    ICO.camara,
    voz.camara ? 'Desligar a câmara' : 'Ligar a câmara',
    () => $('#btn-camara').click(),
    { cortado: !voz.camara },
  ));
  // O de esconder as fotinhas fica NO MEIO, como pedido: é o que separa os controlos de
  // quem fala dos que fecham a transmissão e a chamada.
  barra.append(botaoDoPalco(
    ICO.gente,
    gentePorBaixoOculta ? 'Mostrar quem está na chamada' : 'Ocultar quem está na chamada',
    () => { gentePorBaixoOculta = !gentePorBaixoOculta; desenharVoz(); },
    { cortado: gentePorBaixoOculta },
  ));
  if (souEu) {
    barra.append(botaoDoPalco(ICO.parar, 'Parar a transmissão',
      () => alternarEcra(), { classe: 'palco__bt--parar' }));
  }
  barra.append(botaoDoPalco(
    ICO.desligar,
    souEu ? 'Sair da chamada (fecha a transmissão)' : 'Sair da chamada',
    () => sairDeVoz(),
    { classe: 'palco__bt--sair' },
  ));
  vidro.append(barra);

  // --- a pausa por falta de foco, só para quem transmite
  //
  // A transmissão continua a sair; o que descansa é o DESCODIFICADOR local. Quem partilha
  // está a codificar o ecrã inteiro e, ao mesmo tempo, a descodificá-lo outra vez só para
  // se ver — e quando muda de janela nem sequer está a olhar. Pausar aqui devolve esse
  // trabalho ao jogo, ou ao que quer que ele tenha ido fazer.
  // A MENSAGEM não depende de haver imagem: ela é verdadeira mal a transmissão comece, e
  // prendê-la ao <video> deixava quem mudasse de janela nos primeiros segundos sem
  // explicação nenhuma para o ecrã escuro.
  if (souEu && !janelaComFoco) {
    const pausa = elemento('div', 'palco__pausa');
    pausa.append(elemento('b', null, 'A tua transmissão continua ligada!'));
    pausa.append(elemento('span', null,
      'Pausámos esta pré-visualização para poupar os teus recursos.'));
    vidro.append(pausa);
  }
  if (souEu && el) {
    if (!janelaComFoco) {
      el.__pausaPedida = true;
      try { el.pause(); } catch (e) { /* já parado */ }
    } else if (el.__pausaPedida) {
      el.__pausaPedida = false;
      // Ao voltar, salta-se logo para a ponta: o que passou enquanto estava pausado não
      // interessa a ninguém, e reproduzi-lo seria voltar ao atraso que se acabou de curar.
      try {
        if (el.buffered.length) el.currentTime = el.buffered.end(el.buffered.length - 1) - 0.4;
      } catch (e) { /* o perseguidor do vivo trata */ }
      el.play().catch(() => {});
    }
  }

  palco.append(vidro);

  // --- as fotinhas por baixo
  const fila = elemento('div', 'palco__gente');
  fila.hidden = gentePorBaixoOculta;
  fila.append(fotinha(voz.eu, quem));
  for (const p of outros) fila.append(fotinha(p, quem));
  palco.append(fila);
  return palco;
}

/** Uma fotinha da fila de baixo. `noPalco` é quem está em grande, para não se repetir. */
function fotinha(chave, noPalco) {
  const m = elemento('div', 'mini');
  m.dataset.chave = chave;
  if (voz.falando.has(chave)) m.classList.add('is-speaking');

  const transmite = voz.aPartilhar.has(chave) || (chave === voz.eu && !!voz.ecra);
  const temCamara = voz.comCamara.has(chave) || (chave === voz.eu && !!voz.camara);

  if (temCamara) {
    const cam = chave === voz.eu ? meuEspelho() : camaraDe(chave).tela;
    if (cam) m.append(cam);
  } else {
    const av = elemento('span', 'ident');
    pintar(av, chave);
    m.append(av);
  }

  if (transmite) {
    m.append(elemento('span', 'mini__vivo', 'AO VIVO'));
    // Quem transmite mas NÃO está no palco: diz-se, e o botão de assistir só aparece com o
    // rato por cima — para a fila não virar uma parede de botões.
    if (chave !== noPalco) {
      const tampa = elemento('div', 'mini__tampa');
      tampa.append(elemento('span', null,
        chave === voz.eu ? 'estás a transmitir' : nomeDoPeer(chave) + ' está a transmitir'));
      const b = elemento('button', 'btn btn--primary',
        chave === voz.eu ? 'Ver a tua' : 'Assistir');
      b.onclick = ev => { ev.stopPropagation(); assistir(chave); };
      tampa.append(b);
      m.append(tampa);
    }
  }
  const partida = vozPartida.get(chave);
  if (partida) {
    const aviso = elemento('span', 'mini__vivo', 'SEM ÁUDIO');
    aviso.style.background = 'var(--amber)';
    aviso.style.color = '#16181c';
    aviso.title = partida;
    m.append(aviso);
  }
  m.append(elemento('span', 'mini__nome', nomeDoPeer(chave)));
  return m;
}

/** Dimensiona a grelha da chamada.
 *
 *  O CSS sozinho não chega: com `auto-fit` uma pessoa sozinha ficava com um painel do
 *  tamanho do ecrã e a foto perdida no meio. Aqui calcula-se quantas colunas fazem sentido
 *  e qual o lado máximo que ainda deixa todas as linhas caberem na altura disponível —
 *  portanto os painéis encolhem sozinhos à medida que as pessoas entram, e nunca há scroll.
 */
function ajustarGrelha(n) {
  const g = $('#voz-grelha');
  if (!g || n < 1) return;
  const ESPACO = 12;
  const RACIO = 16 / 10;
  const MAXIMO = 460;   // uma pessoa sozinha não precisa de um painel gigante

  const colunas = Math.ceil(Math.sqrt(n));
  const linhas = Math.ceil(n / colunas);
  const alturaUtil = (g.clientHeight - (linhas - 1) * ESPACO) / linhas;
  const larguraUtil = (g.clientWidth - (colunas - 1) * ESPACO) / colunas;
  const lado = Math.min(alturaUtil * RACIO, larguraUtil, MAXIMO);

  g.style.setProperty('--colunas', colunas);
  g.style.setProperty('--lado', `${Math.max(140, Math.floor(lado))}px`);
}

/** Os botões que aparecem ao passar o rato num painel — e só os que fazem sentido para
 *  aquela pessoa naquele momento. */
function accoesDoPainel(chave, { transmite, aVer, temVideo }) {
  const barra = elemento('div', 'tile__acoes');
  const botao = (rotulo, titulo, accao, ligado) => {
    const b = elemento('button', ligado ? 'tile__bt is-on' : 'tile__bt', rotulo);
    b.title = titulo;
    b.onclick = ev => { ev.stopPropagation(); accao(); };
    return b;
  };

  if (transmite && chave !== voz.eu) {
    barra.append(aVer
      ? botao('▣', 'Voltar à sala', pararDeAssistir)
      : botao('▸', 'Assistir à transmissão', () => assistir(chave)));
  }

  if (aVer && temVideo) {
    barra.append(botao('⛶', 'Ecrã inteiro', () => {
      const v = document.querySelector(`.tile[data-chave="${chave}"] video`);
      if (v) (document.fullscreenElement ? document.exitFullscreen() : v.requestFullscreen());
    }));
  }

  if (chave !== voz.eu) {
    const mudo = voz.silenciados.has(chave);
    barra.append(botao(mudo ? '🔇' : '🔊', mudo ? 'Voltar a ouvir' : 'Silenciar esta pessoa', () => {
      if (mudo) voz.silenciados.delete(chave); else voz.silenciados.add(chave);
      // Baixa-se o ganho dessa pessoa e mais nada: silenciar alguém é uma decisão de quem
      // ouve, e não deve mexer no que os outros recebem.
      ajustarVolume(chave);
      desenharVoz();
    }, mudo));
  }

  return barra;
}

function desenharVoz() {
  const s = servidor();
  const canal = s && s.canais.find(c => c.id === canalAtual);
  const eDeVoz = canal && canal.tipo === 'voz';
  $('#vista-voz').hidden = !eDeVoz;
  desenharNaChamada();
  if (!eDeVoz) return;

  const ligado = voz.canal === canal.id;
  const outros = [...voz.presentes.entries()].filter(([, c]) => c === canal.id).map(([p]) => p);

  const grelha = $('#voz-grelha');
  grelha.textContent = '';
  grelha.classList.toggle('esta-a-ver', !!voz.aVer);

  if (!ligado) {
    const v = elemento('div', 'vazio');
    v.append(elemento('h3', null, canal.nome));
    v.append(elemento('p', null, 'Entra para falar e partilhar o ecrã com quem estiver aqui.'));
    const b = elemento('button', 'btn btn--primary', 'Entrar na sala');
    b.onclick = () => entrarEmVoz(s.id, canal.id);
    v.append(b);
    grelha.append(v);
    $('#voz-nota').textContent = '';
    return;
  }

  // A ver a transmissão de alguém: a imagem ocupa tudo e o resto flutua por cima.
  if (voz.aVer) {
    grelha.append(palcoDeTransmissao(voz.aVer, canal, outros));
    $('#voz-nota').textContent = '';
    return;
  }

  ajustarGrelha(outros.length + 1);
  grelha.append(painelDeVoz(voz.eu));
  for (const p of outros) grelha.append(painelDeVoz(p));

  // Já não há nada para configurar: a voz e o ecrã vão os dois pelo iroh, que trata do
  // NAT sozinho. Esta nota chegou a explicar como pôr um TURN a funcionar; hoje seria
  // explicar um problema que deixou de existir.
  $('#voz-nota').textContent = '';
}

/** O chat da sala de voz, na coluna da direita.
 *
 *  É um canal como os outros — as mensagens ficam no mesmo registo assinado, com o id da
 *  sala de voz como canal, portanto ficam mesmo separadas das dos canais de texto e o
 *  histórico sobrevive a sair e voltar.
 *
 *  O que aqui se faz é escondê-lo de quem não está na sala. Convém dizer com todas as
 *  letras o que isso é e o que não é: é uma regra desta app, não da criptografia. A
 *  mensagem viaja cifrada com a chave do servidor, a mesma de tudo o resto, por isso
 *  chega ao computador de todos os membros e um cliente modificado conseguia lê-la sem
 *  nunca entrar na sala. Para ser garantia a sério a sala precisava de chave própria —
 *  está dito no painel do "?" ao lado do título, para ninguém confiar a mais.
 */
async function desenharChatDaSala() {
  const alvo = $('#sala-chat');
  if (!alvo) return;
  if (!voz.canal || !voz.servidor) { alvo.hidden = true; return; }

  const s = vista.servidores.find(x => x.id === voz.servidor);
  const canal = s && s.canais.find(c => c.id === voz.canal);
  if (!canal) { alvo.hidden = true; return; }

  alvo.hidden = false;
  $('#sala-chat-nome').textContent = `Chat · ${canal.nome}`;
  $('#sala-entrada').placeholder = `Mensagem para ${canal.nome}`;

  const fluxo = $('#sala-fluxo');
  const colado = fluxo.scrollHeight - fluxo.scrollTop - fluxo.clientHeight < 40;
  const msgs = await invoke('mensagens', { servidor: s.id, canal: canal.id }).catch(() => []);

  fluxo.textContent = '';
  if (!msgs.length) {
    fluxo.append(elemento('div', 'salachat__vazio',
      'Só quem está nesta sala vê este chat.'));
    return;
  }
  for (const m of msgs) {
    const linha = elemento('div', 'salachat__msg');
    linha.append(elemento('span', 'salachat__quem', m.autor_nome));
    linha.append(elemento('span', 'salachat__txt', m.texto));
    fluxo.append(linha);
  }
  // Só se salta para o fim se já lá estavas: senão roubava-te a leitura a meio.
  if (colado) fluxo.scrollTop = fluxo.scrollHeight;
}

$('#sala-entrada').addEventListener('keydown', async ev => {
  if (ev.key !== 'Enter' || !ev.target.value.trim()) return;
  if (!voz.canal || !voz.servidor) return;
  const texto = ev.target.value;
  ev.target.value = '';
  try {
    await invoke('enviar', { servidor: voz.servidor, canal: voz.canal, texto });
    await desenharChatDaSala();
  } catch (e) { console.error(e); }
});

/** A lista lateral de quem está na chamada, com o anel verde de quem fala. */
function desenharNaChamada() {
  desenharChatDaSala();
  // Na chamada, a coluna da direita é só da chamada: quem lá está e o chat da sala. A
  // lista geral de membros volta assim que saíres — ali dentro não acrescentava nada e
  // roubava a altura ao chat.
  const membros = $('#bloco-membros');
  if (membros) membros.hidden = !!voz.canal;

  const alvo = $('#na-chamada');
  if (!alvo) return;
  if (!voz.canal) { alvo.hidden = true; alvo.textContent = ''; return; }

  const s = vista.servidores.find(x => x.id === voz.servidor);
  const canal = s && s.canais.find(c => c.id === voz.canal);
  const gente = [voz.eu, ...[...voz.presentes.entries()]
    .filter(([, c]) => c === voz.canal).map(([p]) => p)];

  alvo.hidden = false;
  alvo.textContent = '';
  alvo.append(elemento('div', 'members__label',
    `Na chamada · ${canal ? canal.nome : ''}`));

  for (const p of gente) {
    const linha = elemento('div', 'member member--chamada');
    linha.dataset.chave = p;
    if (voz.falando.has(p)) linha.classList.add('a-falar');
    const av = elemento('span', 'ident');
    pintar(av, p);
    const bloco = elemento('span');
    bloco.append(elemento('b', null, nomeDoPeer(p)));
    const transmite = voz.aPartilhar.has(p) || (p === voz.eu && !!voz.ecra);
    bloco.append(elemento('i', null, transmite ? 'a transmitir' : 'na chamada'));
    linha.append(av, bloco);
    if (transmite) {
      const b = elemento('button', 'chan__x chan__x--ver', '▸');
      b.title = p === voz.eu ? 'Ver o que estás a enviar' : 'Assistir';
      b.onclick = ev => { ev.stopPropagation(); assistir(p); };
      linha.append(b);
    }
    alvo.append(linha);
  }
}

/* A captura pode morrer DEPOIS de o comando já ter dito que sim — uma definição que este
   Windows não tem, o ecrã a desaparecer, o codificador a recusar. Sem isto, a interface
   ficava a dizer "estás a partilhar" para sempre, sem imagem, sem explicação e sem forma
   de a pessoa perceber que o problema não era a ligação dela. */
/* O que não impede a partilha mas a pessoa tem de saber — hoje, um Windows que não deixa
   separar o som da app do resto e portanto devolve a voz da chamada. Fica no botão, que é
   onde ela vai olhar, e não numa consola que não existe. */
listen('partilha-aviso', ev => {
  partilhaAviso = String(ev.payload || '');
  console.warn('aviso da partilha:', partilhaAviso);
  desenharRodape();
});

/** Um aviso sobre a partilha em curso, que não a impede. */
let partilhaAviso = null;

listen('partilha-falhou', ev => {
  const razao = String(ev.payload || 'a captura parou');
  console.warn('a partilha falhou:', razao);
  if (voz.vejoMeuEcra) { voz.vejoMeuEcra.fechar(); voz.vejoMeuEcra = null; }
  if (voz.aVer === voz.eu) voz.aVer = null;
  voz.ecra = null; voz.ecraTamanho = null; voz.qualidadeEmUso = null;
  partilhaFalhou = razao;
  invoke('capacidades', { linha: `partilha-falhou chegou a UI: ${razao}` }).catch(() => {});
  invoke('parar_de_partilhar').catch(() => {});
  anunciarEstado();
  desenharVoz();
  desenharRodape();
});

/** A última razão por que a partilha morreu, para o botão a poder mostrar. */
let partilhaFalhou = null;

listen('presenca', ev => {
  const { peer, canal } = ev.payload;
  if (canal) voz.presentes.set(peer, canal); else voz.presentes.delete(peer);
  // Já não há ligação nenhuma a abrir: quem chega passa a existir para nós assim que o
  // primeiro pedaço de voz dele aparecer, e o Rust já tem a ligação do iroh de pé.
  if (voz.canal && canal === voz.canal) anunciarEstado();
  if (!canal || canal !== voz.canal) {
    if (voz.aSerVistoPor.delete(peer)) actualizarEspectadores();
    fecharFluxoRecebido(peer);
    // E a câmara também. Quem cai da rede não chega a anunciar que a desligou, e sem isto
    // o descodificador dele ficava aberto para sempre a segurar memória de vídeo — numa
    // sala onde as pessoas entram e saem, isso só cresce.
    fecharCamaraRecebida(peer);
    voz.comCamara.delete(peer);
    voz.entendeCamara.delete(peer);
    voz.entendeSom.delete(peer);
    voz.jaFalou.delete(peer);
  }
  if (!canal || canal !== voz.canal) calarPeer(peer);
  desenharVoz();
  desenharCanais();
  desenharRodape();
});

listen('sinal', ev => {
  const { de, canal, dados } = ev.payload;
  if (canal !== voz.canal) return;
  try { receberSinal(de, JSON.parse(dados)); } catch (e) { console.error(e); }
});

/* ==========================================================================
   Rodapé: o que tens aberto, a ligação de voz, e os botões ao lado do nome.
   ========================================================================== */

let jogoAberto = null;

/** Quantas vezes já perguntámos ao Windows o que está aberto.
 *
 *  Existe para o `--medir-ui` poder provar que o interruptor "não olhar para as minhas
 *  janelas" cala mesmo a pergunta. Sem isto, a única forma de verificar era ler o código e
 *  acreditar -- e um interruptor de privacidade é precisamente onde acreditar não chega. */
let perguntasSobreJanelas = 0;

/* --- o que tens aberto ----------------------------------------------------- */

async function verJogo() {
  // Quem desliga isto está a pedir que não se olhe para as janelas dele. Esconder o cartão
  // e continuar a perguntar ao Windows o que está aberto seria dar-lhe a aparência da
  // privacidade sem a privacidade -- o pedido é sobre o que se pergunta, não sobre o que
  // se mostra. Por isso a saída é ANTES do invoke.
  if (deteccaoDeJogoDesligada()) {
    jogoAberto = null;
    $('#jogo').hidden = true;
    return;
  }
  try {
    perguntasSobreJanelas += 1;
    const j = await invoke('jogo_em_execucao');
    jogoAberto = j;
    const linha = $('#jogo');
    if (!j) { linha.hidden = true; return; }
    linha.hidden = false;
    $('#jogo-nome').textContent = j.titulo;
    pintar($('#jogo-marca'), j.processo);
    const aTransmitir = !!voz.ecra;
    $('#jogo-estado').textContent = aTransmitir ? 'A transmitir' : 'Não estás a transmitir';
    $('#btn-jogo').classList.toggle('is-on', aTransmitir);
    $('#btn-jogo').title = aTransmitir
      ? 'Parar de transmitir'
      : `Transmitir — escolhe "${j.titulo}" na janela que aparece`;
  } catch (e) {
    $('#jogo').hidden = true;
  }
}

$('#btn-jogo').onclick = async () => {
  // Não dá para começar a partilhar sem estar numa sala: não haveria a quem enviar.
  if (!voz.canal) {
    const s = servidor();
    const sala = s && s.canais.find(c => c.tipo === 'voz');
    if (!sala) return;
    canalAtual = sala.id;
    desenharCanais();
    desenharTopo();
    await desenharMensagens();
    await entrarEmVoz(s.id, sala.id);
  }
  if (voz.ecra) { alternarEcra(); return; }   // já a transmitir: o botão pára
  // O monitor-com-seta promete transmitir O JOGO, não abrir um menu: procura-se a
  // janela dele pelo título e só se cai no seletor se ela tiver desaparecido.
  const fontes = await invoke('fontes_de_partilha').catch(() => []);
  const doJogo = jogoAberto && fontes.find(f => f.tipo === 'janela' && f.titulo === jogoAberto.titulo);
  if (doJogo) iniciarPartilha(doJogo.id);
  else escolherFonte();
};

setInterval(verJogo, 5000);

/* --- ligação de voz --------------------------------------------------------- */

/** A qualidade da ligação, medida pelo próprio transporte.
 *
 *  Isto vinha das estatísticas do WebRTC. Agora vem do iroh, e é melhor informação: além
 *  do tempo de ida e volta, ele sabe dizer se a ligação é **direta** ou se está a passar
 *  por um relay — que é a diferença entre o router ter sido furado ou não, e a coisa mais
 *  útil que se pode mostrar a quem está a queixar-se de que "está lento".
 */
async function qualidadeDaLigacao() {
  const gente = [...voz.presentes.entries()].filter(([, c]) => c === voz.canal).map(([p]) => p);
  if (!gente.length) return { ok: true, texto: 'Voz conectada' };

  const estado = await invoke('qualidade', { peers: gente }).catch(() => null);
  if (!estado || !estado.length) return { ok: false, texto: 'A ligar…' };

  const relay = estado.some(e => e.relay);
  const pior = Math.max(0, ...estado.map(e => e.ms || 0));
  if (!pior) return { ok: true, texto: relay ? 'Voz conectada · por relay' : 'Voz conectada' };
  return {
    ok: pior < 250 && !relay,
    texto: `Voz conectada · ${Math.round(pior)} ms${relay ? ' · por relay' : ''}`,
  };
}

async function desenharRodape() {
  const ligado = !!voz.canal;
  $('#ligacao').hidden = !ligado;

  if (ligado) {
    const s = vista.servidores.find(x => x.id === voz.servidor);
    const canal = s && s.canais.find(c => c.id === voz.canal);
    $('#ligacao-onde').textContent = canal && s ? `${canal.nome} / ${s.nome}` : '—';

    const q = await qualidadeDaLigacao();
    $('#ligacao-estado').textContent = q.texto;
    $('#ligacao-estado').classList.toggle('is-fraco', !q.ok);
    $('#ligacao-sinal').classList.toggle('is-fraco', !q.ok);

    $('#btn-partilhar').classList.toggle('is-on', !!voz.ecra);
    $('#btn-partilhar').classList.toggle('is-cortado', !!partilhaFalhou);
    $('#btn-partilhar').title = partilhaFalhou
      || partilhaAviso
      || (voz.ecra ? 'Parar de partilhar' : 'Partilhar ecrã');
    $('#btn-partilhar').classList.toggle('is-avisado', !partilhaFalhou && !!partilhaAviso);
    $('#btn-camara').disabled = false;
    $('#btn-camara').classList.toggle('is-on', !!voz.camara);
    $('#btn-camara').classList.toggle('is-cortado', !!camaraFalhou);
    // A razão da falha GANHA ao texto normal. Sem isto, o rodapé — que se redesenha de
    // três em três segundos — apagava a explicação logo a seguir ao clique, e a pessoa
    // ficava com um botão que não faz nada e não diz porquê.
    $('#btn-camara').title = camaraFalhou
      || (voz.camara ? 'Desligar a câmara' : 'Ligar a câmara');
    $('#btn-ruido').classList.toggle('is-cortado', !ruidoSuprimido);
    $('#btn-ruido').title = ruidoSuprimido
      ? 'Supressão de ruído ligada'
      : 'Supressão de ruído desligada';
  }

  const t = voz.micro ? voz.micro.getAudioTracks()[0] : null;
  $('#btn-mic').classList.toggle('is-cortado', (!!t && !t.enabled) || !!vozFalhou);
  // A razão da avaria GANHA ao texto normal, como no botão da câmara.
  $('#btn-mic').title = vozFalhou
    || (!t ? 'Sem microfone' : (t.enabled ? 'Silenciar microfone' : 'Ligar microfone'));
  $('#btn-surdo').classList.toggle('is-cortado', surdo);
  $('#btn-surdo').title = surdo ? 'Voltar a ouvir' : 'Silenciar tudo';
}

/* --- botões ---------------------------------------------------------------- */

let surdo = false;
let ruidoSuprimido = true;

$('#btn-mic').onclick = () => {
  const t = voz.micro ? voz.micro.getAudioTracks()[0] : null;
  if (t) { t.enabled = !t.enabled; desenharVoz(); desenharRodape(); }
};

$('#btn-surdo').onclick = () => {
  // Ficar surdo silencia tudo o que entra E o próprio microfone, como no Discord:
  // não faz sentido continuar a falar para quem não se consegue ouvir a responder.
  surdo = !surdo;
  ajustarTodosOsVolumes();
  const t = voz.micro ? voz.micro.getAudioTracks()[0] : null;
  if (t && surdo) t.enabled = false;
  desenharVoz();
  desenharRodape();
};

$('#btn-ruido').onclick = async () => {
  ruidoSuprimido = !ruidoSuprimido;
  const t = voz.micro ? voz.micro.getAudioTracks()[0] : null;
  if (t) {
    try {
      await t.applyConstraints({
        noiseSuppression: ruidoSuprimido,
        echoCancellation: ruidoSuprimido,
        autoGainControl: ruidoSuprimido,
      });
    } catch (e) {
      console.warn('o microfone não aceitou a mudança:', e);
    }
  }
  desenharRodape();
};

$('#btn-desligar').onclick = () => sairDeVoz();
$('#btn-partilhar').onclick = () => alternarEcra();

/** Porque é que a câmara não abriu, se não abriu. Sobrevive aos redesenhos do rodapé. */
let camaraFalhou = null;

$('#btn-camara').onclick = async () => {
  camaraFalhou = null;
  if (voz.camara) {
    pararDeEnviarCamara();
    anunciarEstado();
    desenharVoz();
    desenharRodape();
    return;
  }
  try {
    await comecarAEnviarCamara();
  } catch (e) {
    // Recusar a permissão é uma resposta, não uma avaria. Mas tem de SE VER: um aviso só
    // na consola é o mesmo que não haver aviso nenhum para quem carregou no botão.
    console.warn('a câmara não abriu:', e);
    camaraFalhou = `A câmara não abriu: ${e && e.message ? e.message : e}`;
    desenharRodape();
    return;
  }
  anunciarEstado();
  desenharVoz();
  desenharRodape();
};

// A ligação muda de qualidade sozinha; o rodapé acompanha.
setInterval(() => { if (voz.canal) desenharRodape(); }, 3000);

/* ==========================================================================
   Vista de chamada: quem está, quem fala, e quem transmite.
   ========================================================================== */

/** Deteção de fala.
 *
 *  Não se pergunta ao WebRTC se alguém está a falar — ele não sabe. Mede-se a energia
 *  do áudio com a Web Audio API, e usa-se histerese: entra em "a falar" acima de um
 *  limiar e só sai abaixo de outro mais baixo. Sem isso o anel verde pisca em cada
 *  pausa entre sílabas, que é pior do que não o ter.
 */
const LIMIAR_ENTRA = 0.045;
const LIMIAR_SAI = 0.022;

/** O anel verde de quem fala, medido no som que se acabou de descodificar.
 *
 *  Antes havia um analisador da Web Audio por pessoa, ligado ao fluxo do WebRTC. Agora as
 *  amostras já passam por aqui a caminho dos altifalantes — medi-las outra vez num
 *  analisador em paralelo seria fazer o mesmo trabalho duas vezes.
 *
 *  A histerese fica: entra-se em "a falar" acima de um limiar e só se sai abaixo de outro
 *  mais baixo. Sem isso o anel pisca em cada pausa entre sílabas, que é pior do que não o
 *  ter.
 */
function medirNasAmostras(chave, amostras) {
  let soma = 0;
  for (let i = 0; i < amostras.length; i++) soma += amostras[i] * amostras[i];
  const rms = Math.sqrt(soma / Math.max(1, amostras.length));

  const estava = voz.falando.has(chave);
  const agora = estava ? rms > LIMIAR_SAI : rms > LIMIAR_ENTRA;
  if (agora === estava) return;

  if (agora) voz.falando.add(chave); else voz.falando.delete(chave);
  document.querySelectorAll(`[data-chave="${chave}"]`).forEach(el => {
    el.classList.toggle('a-falar', agora);
  });
}

function vigiarAudio(chave, stream) {
  pararDeVigiar(chave);
  if (!stream || !stream.getAudioTracks().length) return;
  try {
    if (!voz.audioCtx) voz.audioCtx = new AudioContext();
    if (voz.audioCtx.state === 'suspended') voz.audioCtx.resume();
    const fonte = voz.audioCtx.createMediaStreamSource(stream);
    const an = voz.audioCtx.createAnalyser();
    an.fftSize = 512;
    an.smoothingTimeConstant = 0.4;
    fonte.connect(an);
    voz.analisadores.set(chave, { an, fonte, dados: new Float32Array(an.fftSize) });
  } catch (e) {
    console.warn('não consegui vigiar o áudio de', chave, e);
  }
}

function pararDeVigiar(chave) {
  const a = voz.analisadores.get(chave);
  if (!a) return;
  try { a.fonte.disconnect(); } catch (e) { /* já desligado */ }
  voz.analisadores.delete(chave);
  voz.falando.delete(chave);
}

function medirFala() {
  if (!voz.canal || !voz.analisadores.size) return;
  for (const [chave, a] of voz.analisadores) {
    a.an.getFloatTimeDomainData(a.dados);
    let soma = 0;
    for (let i = 0; i < a.dados.length; i++) soma += a.dados[i] * a.dados[i];
    const rms = Math.sqrt(soma / a.dados.length);

    // O microfone silenciado nunca "fala", por mais barulho que haja na sala.
    const proprioSilenciado =
      chave === voz.eu && (!voz.micro || !voz.micro.getAudioTracks()[0]?.enabled);

    const estava = voz.falando.has(chave);
    const agora = proprioSilenciado ? false : (estava ? rms > LIMIAR_SAI : rms > LIMIAR_ENTRA);
    if (agora === estava) continue;

    if (agora) voz.falando.add(chave); else voz.falando.delete(chave);
    // Mexe-se nas classes diretamente: redesenhar tudo dez vezes por segundo daria
    // um piscar constante e mataria a lista de mensagens.
    document.querySelectorAll(`[data-chave="${chave}"]`).forEach(el => {
      el.classList.toggle('a-falar', agora);
    });
  }
}
setInterval(medirFala, 120);

// A janela muda de tamanho, os painéis acompanham.
addEventListener('resize', () => { if (voz.canal) desenharVoz(); });

/* --- estado partilhado entre peers ----------------------------------------- */

/** Diz a toda a gente na sala o que estou a enviar.
 *
 *  Quem recebe um fluxo de vídeo não consegue saber, do lado de lá, se aquilo é um ecrã ou
 *  uma câmara — os bytes são os mesmos. Portanto quem envia é que tem de contar.
 */
/** O que esta versão sabe falar.
 *
 *  # Porque é que isto teve de existir
 *
 *  Duas mudanças de hoje partem o entendimento com quem ainda não actualizou, e as duas
 *  falham em SILÊNCIO — que é a pior maneira de falhar:
 *
 *  1. A **câmara** vai pelo mesmo transporte do ecrã, distinguida por um campo novo no
 *     cabeçalho. O serde ignora campos que não conhece, portanto uma versão anterior aceita
 *     o cabeçalho e mete os frames da câmara no caminho do ECRÃ. Não estoira: mostra lixo.
 *  2. O **som na partilha de ecrã** vem como segunda faixa no contentor, e uma versão
 *     anterior só sabe traduzir uma. Resultado: ecrã preto, sem nenhuma mensagem de erro.
 *
 *  Não havia forma de as duas pontas saberem com quem estavam a falar. Passa a haver: cada
 *  lado diz um número, e quem não o diz é antigo. `1` é implícito — é o que as versões que
 *  nunca souberam disto valem.
 */
const PROTOCOLO = 2;

/** "1440p 60FPS" — como o Discord o escreve, e a partir dos números que valem mesmo.
 *
 *  Mostra-se sempre o MEDIDO e não o pedido, porque é o medido que a outra pessoa recebe.
 *  Com "Nativa" o pedido é zero, e mesmo com um número o Rust arredonda para blocos pares.
 */
function rotuloDaQualidade() {
  const q = voz.qualidadeEmUso || qualidadeEfetiva();
  const alt = (voz.ecraTamanho && voz.ecraTamanho.altura) || q.altura;
  return `${alt ? alt + 'p' : 'Nativa'} ${q.fps}FPS`;
}

/** Porque é que o número não é o que a pessoa escolheu, quando não é.
 *
 *  A resolução do menu é um TETO, não um alvo: nunca amplia. Quem partilha uma janela de
 *  1050 de altura e escolheu 1440p vê "1050p" e fica sem perceber se a escolha funcionou.
 *  Devolve `null` quando não há nada a explicar.
 */
function porqueEstaResolucao() {
  const q = voz.qualidadeEmUso;
  if (!q || !voz.ecraTamanho) return null;
  const real = voz.ecraTamanho.altura;
  if (!q.altura) return `a fonte tem ${real} de altura e vai como está (escolheste Nativa)`;
  if (real < q.altura) {
    return `escolheste ${q.altura}p, mas a fonte só tem ${real} de altura — a escolha é um`
      + ` limite, não aumenta o que não existe`;
  }
  // Aqui só se sabe o tamanho de SAÍDA, não o da fonte — portanto não se pode afirmar que
  // houve redução. Dizer "foi reduzida" quando a fonte já era desse tamanho seria inventar.
  // Uma frase vaga e sempre verdadeira vale mais do que uma precisa e às vezes falsa.
  return `vai a ${voz.ecraTamanho.largura}x${real}, dentro do limite de ${q.altura}p que`
    + ` escolheste`;
}

function anunciarEstado() {
  for (const [peer, c] of voz.presentes) {
    if (c !== voz.canal) continue;
    sinalizar(peer, {
      tipo: 'estado', ecra: !!voz.ecra, camara: !!voz.camara, v: PROTOCOLO,
      // Quem transmite é o único que sabe estas duas coisas. Sem as dizer, quem assiste
      // teria de as adivinhar — e a barra mostraria um palpite em vez de um facto.
      qualidade: voz.ecra ? rotuloDaQualidade() : null,
      espectadores: voz.ecra ? voz.aSerVistoPor.size : 0,
    });
  }
}

/* --- assistir a uma transmissão -------------------------------------------- */

function deixarDeVerOMeu() {
  if (!voz.vejoMeuEcra) return;
  invoke('parar_de_ver_meu_ecra').catch(() => {});
  voz.vejoMeuEcra.fechar();
  voz.vejoMeuEcra = null;
}

function assistir(peer) {
  if (voz.aVer && voz.aVer !== peer && voz.aVer !== voz.eu) {
    sinalizar(voz.aVer, { tipo: 'assistir', ligado: false });
  }
  if (voz.aVer === voz.eu && peer !== voz.eu) deixarDeVerOMeu();
  voz.aVer = peer;
  if (peer === voz.eu) {
    // O fluxo nasce AGORA, e só agora é que o Rust abre a torneira — reenviando o
    // princípio da transmissão primeiro, para o <video> saber o que vai receber.
    voz.vejoMeuEcra = fluxoDePedacos();
    desenharVoz();                       // o <video> entra no DOM antes dos pedaços
    invoke('ver_meu_ecra').catch(() => {});
    return;
  }
  // Quem transmite tem de saber que estou a ver: é essa lista que decide o que sai da
  // máquina dele. Sem isto o ecrã era codificado para ninguém.
  sinalizar(peer, { tipo: 'assistir', ligado: true });
  desenharVoz();
}

function pararDeAssistir() {
  if (voz.aVer === voz.eu) deixarDeVerOMeu();
  else if (voz.aVer) sinalizar(voz.aVer, { tipo: 'assistir', ligado: false });
  voz.aVer = null;
  desenharVoz();
}

/* ---------- arranque ---------- */


(async () => {
  voz.eu = await invoke('meu_endereco').catch(() => null);
  await desenharTudo();
  if (!vista.nome) {
    abrir('veu-bemvindo');
    $('#in-nome').focus();
  }
  procurarAtualizacao();
  verJogo();
  desenharRodape();
})();

/* ---------- o que esta webview consegue descodificar ----------------------- */

/* O ecrã vai passar a chegar como H.264 nosso, descodificado aqui pelo WebCodecs em vez
   de vir por WebRTC. Isso depende da versão da WebView2 que cada pessoa tem instalada, e
   a aceleração por hardware depende ainda da placa — não é coisa para se assumir. */
(async () => {
  const diz = linha => invoke('capacidades', { linha }).catch(() => {});
  if (typeof VideoDecoder === 'undefined') {
    return diz('sem WebCodecs — esta webview não descodifica o ecrã nativo');
  }
  const perfil = {
    codec: 'avc1.640028',      // H.264 High, nível 4.0 — chega para 1080p60
    codedWidth: 1920,
    codedHeight: 1080,
    optimizeForLatency: true,
  };
  // Cuidado com o que isto responde: o `isConfigSupported` diz que a configuração é
  // ACEITE, não que a descodificação vá parar ao hardware — o `prefer-hardware` é uma
  // dica, e a config devolvida limita-se a repetir a preferência pedida. Quem responde a
  // "usou mesmo o hardware" é a utilização do descodificador da GPU, com stream a sério.
  const aceita = async preferencia => {
    try {
      const r = await VideoDecoder.isConfigSupported({ ...perfil, hardwareAcceleration: preferencia });
      return r.supported ? 'aceite' : 'recusado';
    } catch (e) { return `erro: ${e.name}`; }
  };
  diz(`WebCodecs presente · config H.264 1080p: prefere-hardware=${await aceita('prefer-hardware')}`
    + ` prefere-software=${await aceita('prefer-software')} indiferente=${await aceita('no-preference')}`);

  // A voz vai pelo mesmo caminho do ecrã ou continua a precisar de configuração à mão?
  // Depende destas três, e nenhuma se pode assumir.
  const audio = { codec: 'opus', sampleRate: 48000, numberOfChannels: 1, bitrate: 24000 };
  const pergunta = async (classe, nome) => {
    if (typeof classe === 'undefined') return 'não existe';
    try {
      const r = await classe.isConfigSupported(audio);
      return r.supported ? 'aceite' : 'recusado';
    } catch (e) { return `erro: ${e.name}`; }
  };
  diz(`áudio · AudioEncoder=${await pergunta(window.AudioEncoder)}`
    + ` AudioDecoder=${await pergunta(window.AudioDecoder)}`
    + ` MediaStreamTrackProcessor=${typeof window.MediaStreamTrackProcessor === 'undefined' ? 'não existe' : 'existe'}`
    + ` AudioWorklet=${typeof AudioWorkletNode === 'undefined' ? 'não existe' : 'existe'}`);
})();

/* ---------- autoteste do ECO ------------------------------------------------ */

/* A pergunta: quando o Bruma partilha o som do sistema, ele apanha a SUA PRÓPRIA voz?

   Se apanhasse, a voz das outras pessoas na chamada — que sai pelas colunas por ordem do
   Bruma — voltava a entrar na partilha e era reenviada. Quem estivesse do outro lado
   ouvia-se a si próprio, com o atraso do caminho todo.

   Mede-se por diferença, que é a única forma honesta: primeiro com a app calada, depois
   com ela a tocar um tom bem alto por si mesma. Se a captura nos exclui, os dois números
   são praticamente iguais — e se não exclui, o segundo dispara.

   Mede-se por DIFERENÇA e não em absoluto porque a máquina pode ter outra coisa a tocar,
   e essa é para ser captada: é justamente o que a partilha de ecrã deve levar. */
(async () => {
  if (!window.__TAURI__) return;
  if (!(await invoke('autoteste_pedido').catch(() => 0))) return;
  const diz = linha => invoke('capacidades', { linha }).catch(() => {});

  const calado = await invoke('medir_som', { ms: 1200 }).catch(e => ({ erro: String(e) }));
  if (calado.erro) return diz(`autoteste eco: não consegui medir (${calado.erro})`);

  // Um tom nosso, alto, pelo mesmo caminho por onde sai a voz das outras pessoas.
  const ctx = contextoDeAudio();
  const osc = ctx.createOscillator();
  const vol = ctx.createGain();
  osc.frequency.value = 440;
  vol.gain.value = 0.35;
  osc.connect(vol); vol.connect(ctx.destination);
  osc.start();
  const aTocar = await invoke('medir_som', { ms: 1200 }).catch(() => null);
  osc.stop();
  try { osc.disconnect(); vol.disconnect(); } catch (e) { /* já solto */ }
  if (!aTocar) return diz('autoteste eco: a segunda medição falhou');

  // Com o tom a 0.35 de ganho, se ele entrasse na captura o rms subia MUITO. O limiar é
  // generoso de propósito: o que se quer distinguir é "não entrou" de "entrou todo".
  const subiu = aTocar.rms - calado.rms;
  const passou = aTocar.semEco ? subiu < 500 : true;
  diz(`autoteste eco: eu=${aTocar.eu} a tocar agora: ${(aTocar.quem || []).join(', ') || 'ninguem'}`);
  diz(`autoteste eco: eu=${aTocar.eu} | a tocar agora: ${(aTocar.quem || []).join(', ') || 'ninguem'}`);
  diz(`autoteste eco: semEco=${aTocar.semEco}`
    + ` rms calado=${calado.rms.toFixed(0)} a tocar=${aTocar.rms.toFixed(0)}`
    + ` (subiu ${subiu.toFixed(0)}) -> ${aTocar.semEco ? (passou ? 'EXCLUIDO' : 'FALHOU: o nosso som entrou') : 'sem exclusao neste Windows'}`);
})();

/* ---------- autoteste da câmara -------------------------------------------- */

/* Prova o caminho da câmara SEM rede e SEM segunda máquina: abre a câmara (ou, se não
   houver nenhuma, uma tela desenhada por nós), codifica, e mete os pedaços num
   descodificador de verdade. É a mesma ideia do teste de par — verificar cada metade não
   verifica o meio — aplicada ao codec: o que interessa não é "o codificador aceitou a
   configuração", é "saíram bytes que um descodificador conseguiu transformar em imagem".

   O caso sem câmara não é um atalho: prova a codificação e a descodificação em qualquer
   máquina, incluindo as do CI, onde câmara não há nenhuma. */
(async () => {
  if (!window.__TAURI__) return;
  if (!(await invoke('autoteste_pedido').catch(() => 0))) return;
  const diz = linha => invoke('capacidades', { linha }).catch(() => {});

  diz('autoteste câmara: a começar');
  if (typeof VideoEncoder === 'undefined' || typeof VideoDecoder === 'undefined') {
    return diz('autoteste câmara: esta webview não traz VideoEncoder/VideoDecoder');
  }

  /** Um pedido que DESISTE. Sem isto, uma permissão que nunca é respondida deixa o teste
   *  pendurado para sempre — e um teste pendurado parece-se com um teste que não corre. */
  const comPrazo = (promessa, ms, oque) => Promise.race([
    promessa,
    new Promise((_, mal) => setTimeout(() => mal(new Error(`${oque} não respondeu em ${ms} ms`)), ms)),
  ]);

  let fonte = 'câmara';
  let faixa = null;
  let parar = () => {};
  try {
    const dispositivos = await comPrazo(
      navigator.mediaDevices.enumerateDevices(), 4000, 'enumerateDevices');
    const camaras = dispositivos.filter(d => d.kind === 'videoinput');
    diz(`autoteste câmara: ${camaras.length} dispositivo(s): `
      + camaras.map(c => `"${c.label || 'sem nome'}"`).join(', '));
    if (!camaras.length) throw new Error('nenhuma');
    // Duas tentativas, e a diferença entre elas é diagnóstico: se a apertada falha e a
    // solta passa, o problema é a resolução pedida; se falham as duas, é o dispositivo.
    let stream;
    try {
      stream = await comPrazo(navigator.mediaDevices.getUserMedia({
        video: { width: CAM_LARGURA, height: CAM_ALTURA, frameRate: CAM_IPS }, audio: false,
      }), 6000, 'getUserMedia');
    } catch (apertado) {
      diz(`autoteste câmara: com 640x360 falhou (${apertado.message}); a tentar sem exigências`);
      stream = await comPrazo(
        navigator.mediaDevices.getUserMedia({ video: true, audio: false }), 6000, 'getUserMedia');
    }
    faixa = stream.getVideoTracks()[0];
    parar = () => stream.getTracks().forEach(t => t.stop());
    diz(`autoteste câmara: ${camaras.length} dispositivo(s); a usar "${camaras[0].label || 'sem nome'}"`);
  } catch (e) {
    // Sem câmara desenha-se uma: um quadrado que ANDA, porque uma imagem parada
    // comprimiria para quase nada e não provaria que o codificador está a trabalhar.
    fonte = 'tela desenhada';
    const tela = document.createElement('canvas');
    tela.width = CAM_LARGURA; tela.height = CAM_ALTURA;
    const pincel = tela.getContext('2d');
    let x = 0;
    const pintar = () => {
      pincel.fillStyle = '#101418';
      pincel.fillRect(0, 0, tela.width, tela.height);
      pincel.fillStyle = '#7fd4c1';
      pincel.fillRect(x % (tela.width - 60), 40 + (x % 200), 60, 60);
      x += 11;
    };
    const relogio = setInterval(pintar, 1000 / CAM_IPS);
    pintar();
    const stream = tela.captureStream(CAM_IPS);
    faixa = stream.getVideoTracks()[0];
    parar = () => { clearInterval(relogio); stream.getTracks().forEach(t => t.stop()); };
    diz(`autoteste câmara: sem dispositivo (${e.message}); a usar uma tela desenhada`);
  }

  let codificados = 0, bytes = 0, chaves = 0, desenhados = 0, erros = 0;
  let larguraVista = 0, alturaVista = 0;

  const descodificador = new VideoDecoder({
    output: q => {
      desenhados += 1;
      larguraVista = q.displayWidth; alturaVista = q.displayHeight;
      q.close();
    },
    error: e => { erros += 1; console.warn('autoteste câmara, descodificador:', e); },
  });
  descodificador.configure({ codec: 'avc1.42001f', optimizeForLatency: true });

  let esperaChave = true;
  const codificador = new VideoEncoder({
    output: pedaco => {
      codificados += 1;
      const b = new Uint8Array(pedaco.byteLength);
      pedaco.copyTo(b);
      bytes += b.length;
      // O MESMO teste que o caminho real usa, e de propósito: se o `temSPS` estiver
      // errado, este autoteste tem de falhar aqui e não deixar o erro para a chamada.
      const completo = temSPS(b);
      if (completo) chaves += 1;
      if (esperaChave) {
        if (!completo) return;
        esperaChave = false;
      }
      try {
        descodificador.decode(new EncodedVideoChunk({
          type: completo ? 'key' : 'delta',
          timestamp: codificados * 1000,
          data: b,
        }));
      } catch (e) { erros += 1; }
    },
    error: e => { erros += 1; console.warn('autoteste câmara, codificador:', e); },
  });
  codificador.configure({
    codec: 'avc1.42001f', width: CAM_LARGURA, height: CAM_ALTURA,
    framerate: CAM_IPS, bitrate: CAM_DEBITO, latencyMode: 'realtime',
    avc: { format: 'annexb' },
  });

  const leitor = new MediaStreamTrackProcessor({ track: faixa }).readable.getReader();
  const ate = performance.now() + 4000;
  let lidos = 0, ultimaChave = 0;
  while (performance.now() < ate) {
    const { value, done } = await leitor.read().catch(() => ({ done: true }));
    if (done) break;
    lidos += 1;
    const agora = performance.now();
    const chave = agora - ultimaChave >= CAM_CHAVE_MS;
    if (chave) ultimaChave = agora;
    try { codificador.encode(value, { keyFrame: chave }); } catch (e) { erros += 1; }
    value.close();
  }
  try { await codificador.flush(); } catch (e) { /* segue */ }
  await new Promise(r => setTimeout(r, 400));
  try { leitor.cancel(); } catch (e) { /* idem */ }
  parar();
  try { codificador.close(); } catch (e) { /* idem */ }
  try { descodificador.close(); } catch (e) { /* idem */ }

  diz(`autoteste câmara (${fonte}): ${lidos} lidos, ${codificados} codificados`
    + ` (${(bytes / 1024).toFixed(0)} KB, ${chaves} completos),`
    + ` ${desenhados} DESCODIFICADOS a ${larguraVista}x${alturaVista}, ${erros} erros`);
})();

/* ---------- autoteste da partilha de ecrã ---------------------------------- */

/* Corre só com `bruma --autoteste`. Parte do princípio de que nada funciona e vai
   verificando: os pedaços chegam? o vídeo aceita-os? tem dimensões? o tempo anda?
   Cada uma dessas perguntas falha de maneira diferente e em sítios diferentes. */
(async () => {
  if (!window.__TAURI__) return;
  const segundos = await invoke('autoteste_pedido').catch(() => 0);
  if (!segundos) return;

  const diz = linha => invoke('capacidades', { linha }).catch(() => {});
  const fluxo = fluxoDePedacos();
  document.body.append(fluxo.el);          // o MediaSource só anda com o elemento no DOM
  fluxo.el.style.cssText = 'position:fixed;width:2px;height:2px;opacity:0;pointer-events:none';

  let pedacos = 0, bytes = 0;
  const inteiro = [];   // o mesmo vídeo, para o provar por um caminho que não é o MSE
  const canal = new window.__TAURI__.core.Channel();
  canal.onmessage = p => {
    const b = p instanceof ArrayBuffer ? new Uint8Array(p) : new Uint8Array(p);
    pedacos += 1; bytes += b.length;
    if (b.length && b[0] === ETIQUETA_BYTES) inteiro.push(b.subarray(1));
    fluxo.empurrar(b);
  };

  try {
    const r = await invoke('comecar_a_partilhar',
      { servidor: 'autoteste', canalVoz: 'autoteste',
        fonte: await invoke('autoteste_fonte').catch(() => 'ecra:1'),
        altura: await invoke('autoteste_altura').catch(() => 720),
        fps: await invoke('autoteste_fps').catch(() => 30),
        debito: 0, comSom: true, saida: canal });
    await invoke('ver_meu_ecra');   // a pré-visualização é gated: sem isto nada chega
    const fontes = await invoke('fontes_de_partilha').catch(() => []);
    const comImagem = fontes.filter(f => f.miniatura && f.miniatura.length > 2000).length;
    diz(`autoteste fontes: ${fontes.length} no total, ${comImagem} com miniatura`
      + ` (${fontes.slice(0, 4).map(f => f.tipo + ':' + f.titulo.slice(0, 18)).join(' | ')})`);
    diz(`autoteste: a captar a ${r.largura}x${r.altura}`);
  } catch (e) {
    return diz(`autoteste FALHOU a arrancar: ${e}`);
  }

  await new Promise(r => setTimeout(r, segundos * 1000));
  await invoke('parar_de_partilhar').catch(() => {});

  const v = fluxo.el;
  const intervalos = [];
  for (let i = 0; i < v.buffered.length; i++) {
    intervalos.push(`${v.buffered.start(i).toFixed(2)}-${v.buffered.end(i).toFixed(2)}`);
  }
  diz(`autoteste: ${pedacos} pedaços, ${(bytes / 1e6).toFixed(1)} MB`
    + ` | vídeo ${v.videoWidth}x${v.videoHeight}, readyState=${v.readyState}`
    + ` | descodificados=${v.getVideoPlaybackQuality ? v.getVideoPlaybackQuality().totalVideoFrames : '?'}`
    // "sem erro" não prova que o som toca: prova que ninguém se queixou. Estes bytes são o
    // navegador a dizer que DESCODIFICOU som — é a diferença entre a faixa existir e a
    // faixa funcionar, e foi por não a medir que o vídeo já passou por bom estando parado.
    + ` | som descodificado=${v.webkitAudioDecodedByteCount ?? '?'} bytes`
    + ` | bufferizado=[${intervalos.join(', ')}] t=${v.currentTime.toFixed(2)}`
    + ` | erro=${v.error ? v.error.code : 'nenhum'}`
    + (v.error && v.error.message ? ` "${v.error.message}"` : ''));
  fluxo.fechar();

  /* E a segunda prova, por fora do MSE: os mesmos bytes num <video> comum. Se este toca
     e o de cima não, o ficheiro está bom e o problema é do dialeto que o MSE exige; se
     nenhum toca, o problema está antes, no que estamos a produzir. Sem separar as duas
     coisas, o passo seguinte é adivinhar. */
  // ---- a voz, com o circuito fechado aqui mesmo ----------------------------
  // Não dá para provar a voz sozinho de um lado ao outro da rede, mas dá para provar a
  // metade que vive aqui: o microfone é codificado em Opus e descodificado a seguir, e
  // conta-se o que entrou e o que saiu. Se isto não fechar, não vale a pena procurar na
  // rede. O transporte tem prova própria, no `cargo test` do módulo da rede.
  try {
    const mic = await navigator.mediaDevices.getUserMedia({ audio: true });
    let codificados = 0, descodificados = 0, amostras = 0, energia = 0;

    const dec = new AudioDecoder({
      output: som => {
        descodificados += 1;
        amostras += som.numberOfFrames;
        const f = new Float32Array(som.numberOfFrames);
        try {
          som.copyTo(f, { planeIndex: 0, format: 'f32-planar' });
          for (let i = 0; i < f.length; i++) energia += f[i] * f[i];
        } catch (e) { /* formato inesperado */ }
        som.close();
      },
      error: e => console.warn('descodificador:', e),
    });
    dec.configure({ codec: 'opus', sampleRate: 48000, numberOfChannels: 1 });

    const enc = new AudioEncoder({
      output: pedaco => {
        codificados += 1;
        const b = new Uint8Array(pedaco.byteLength);
        pedaco.copyTo(b);
        try {
          dec.decode(new EncodedAudioChunk({
            type: 'key', timestamp: pedaco.timestamp, data: b,
          }));
        } catch (e) { /* segue */ }
      },
      error: e => console.warn('codificador:', e),
    });
    enc.configure({
      codec: 'opus', sampleRate: 48000, numberOfChannels: 1,
      bitrate: 24000, opus: { frameDuration: 20000 },
    });

    const leitor = new MediaStreamTrackProcessor({ track: mic.getAudioTracks()[0] })
      .readable.getReader();
    const fim = Date.now() + 3000;
    while (Date.now() < fim) {
      const { value, done } = await leitor.read();
      if (done) break;
      enc.encode(value);
      value.close();
    }
    await enc.flush();
    await dec.flush();
    leitor.cancel();
    mic.getTracks().forEach(t => t.stop());

    const rms = amostras ? Math.sqrt(energia / amostras) : 0;
    diz(`autoteste voz: ${codificados} pedaços codificados, ${descodificados} descodificados`
      + ` (${(amostras / 48000).toFixed(1)}s de som, rms ${rms.toFixed(4)})`);
  } catch (e) {
    diz(`autoteste voz FALHOU: ${e.name} — ${e.message}`);
  }

  const simples = document.createElement('video');
  simples.muted = true;
  simples.style.cssText = 'position:fixed;width:2px;height:2px;opacity:0';
  document.body.append(simples);
  simples.src = URL.createObjectURL(new Blob(inteiro, { type: 'video/mp4' }));
  await new Promise(r => {
    simples.onloadeddata = r;
    simples.onerror = r;
    setTimeout(r, 5000);
  });
  diz(`autoteste (sem MSE, ficheiro inteiro): ${simples.videoWidth}x${simples.videoHeight}`
    + ` readyState=${simples.readyState} duração=${simples.duration}`
    + ` erro=${simples.error ? simples.error.code : 'nenhum'}`
    + (simples.error && simples.error.message ? ` "${simples.error.message}"` : ''));
  simples.remove();
  v.remove();
})();

/* ---------- autoteste de par: duas instâncias a falar ---------------------- */

/* A voz tem duas metades que se provam sozinhas — o codec e o transporte — e uma que não:
   a do meio. Quem está na sala, o datagrama a sair para a pessoa certa, e o pedaço a
   chegar ao descodificador do outro lado. Isso só se vê com duas instâncias.

     bruma --par              cria o servidor e escreve o convite
     bruma --par=<convite>    entra, junta-se à sala e conta o que ouviu

   Cada uma com o seu BRUMA_DADOS, senão partilham a identidade e não são duas pessoas. */
(async () => {
  if (!window.__TAURI__) return;
  const modo = await invoke('autoteste_par').catch(() => null);
  if (modo === null || modo === undefined) return;

  const diz = linha => invoke('capacidades', { linha }).catch(() => {});
  const esperar = ms => new Promise(r => setTimeout(r, ms));

  try {
    let servidorId;
    if (modo === '') {
      servidorId = await invoke('criar_servidor', { nome: 'par' });
      await invoke('criar_canal', { servidor: servidorId, nome: 'sala', tipo: 'voz' });
      await invoke('criar_canal', { servidor: servidorId, nome: 'geral', tipo: 'texto' });

      // Escrever ANTES de o convidado existir. É a promessa central do projeto — que nada
      // morre por o outro estar offline — e ninguém guardou isto num servidor: fica aqui,
      // à espera de alguém a quem o dar.
      await desenharTudo();
      const texto = vista.servidores.find(x => x.id === servidorId)
        .canais.find(c => c.tipo === 'texto');
      for (let i = 1; i <= 5; i++) {
        await invoke('enviar', { servidor: servidorId, canal: texto.id, texto: `antes ${i}` });
      }
      diz('par ANFITRIAO escreveu 5 mensagens antes de existir convidado');

      const convite = await invoke('criar_convite', { servidor: servidorId });
      diz(`par ANFITRIAO convite=${convite}`);
    } else {
      servidorId = await invoke('entrar_com_convite', { codigo: modo });
      diz('par CONVIDADO entrou');

      // O histórico que existia antes de eu existir tem de chegar cá.
      let msgs = [];
      for (let i = 0; i < 30 && msgs.length < 5; i++) {
        await esperar(500);
        await desenharTudo();
        const srv = vista.servidores.find(x => x.id === servidorId);
        const t = srv && srv.canais.find(c => c.tipo === 'texto');
        if (t) msgs = await invoke('mensagens', { servidor: servidorId, canal: t.id }).catch(() => []);
      }
      diz(`par CONVIDADO recebeu ${msgs.length}/5 mensagens escritas antes de ele entrar`
        + (msgs.length ? ` (primeira: "${msgs[0].texto}", última: "${msgs[msgs.length - 1].texto}")` : ''));
    }

    // Esperar que o canal de voz apareça: o convidado só o conhece depois de sincronizar.
    let canal = null;
    for (let i = 0; i < 40 && !canal; i++) {
      await desenharTudo();
      const srv = vista.servidores.find(x => x.id === servidorId);
      canal = srv && srv.canais.find(c => c.tipo === 'voz');
      if (!canal) await esperar(500);
    }
    if (!canal) return diz('par FALHOU: o canal de voz nunca apareceu');

    servidorAtual = servidorId;
    canalAtual = canal.id;
    await entrarEmVoz(servidorId, canal.id);
    diz(`par entrou na sala (microfone=${voz.micro ? 'sim' : 'não'})`);

    // As DUAS instâncias ligam a câmara. Ao contrário do ecrã, que é um de cada vez, as
    // câmaras são simultâneas — e é precisamente aí que mora o bug que uma instância
    // sozinha nunca mostra: cada lado tem de descodificar o outro enquanto codifica o seu.
    try {
      await comecarAEnviarCamara(camaraDesenhada());
      anunciarEstado();
      diz(`par câmara ligada=${!!voz.camara}`);
    } catch (e) {
      diz(`par câmara NÃO ligou: ${e && e.message ? e.message : e}`);
    }

    // O anfitrião parte o ecrã; o convidado vai assistir. É o caminho que dá sentido ao
    // projeto e o único que nunca tinha sido visto entre dois pares.
    if (modo === '') {
      await esperar(3000);
      // Diretamente, sem o seletor: num teste automatico nao ha quem clique no menu.
      // O teste de par usa a qualidade REAL (a que sai do menu), para exercitar o mesmo
      // caminho do dono. As bandeiras escrevem-na antes, para se poder variar.
      localStorage.setItem('bruma.qualidade', JSON.stringify({
        modo: 'pers',
        altura: await invoke('autoteste_altura').catch(() => 1080),
        fps: await invoke('autoteste_fps').catch(() => 30),
        debito: 0,
        som: true,
      }));
      await iniciarPartilha(await invoke('autoteste_fonte').catch(() => 'ecra:1'));
      anunciarEstado();
      diz(`par ANFITRIAO a partilhar=${!!voz.ecra}`);
    }

    // Deixar correr, e depois contar. O que interessa é `recebidos`: prova que o datagrama
    // saiu de uma instância e chegou ao descodificador da outra.
    for (let volta = 1; volta <= 6; volta++) {
      await esperar(5000);
      const gente = [...voz.presentes.keys()];
      const estado = await invoke('qualidade', { peers: gente }).catch(() => []);
      const resumo = estado.map(e =>
        `${e.peer.slice(0, 6)} ${e.relay ? 'relay' : 'direta'} ↑${e.enviados} ↓${e.recebidos}`
      ).join(' | ');
      const ecra = estado.map(e => `ecrã ↑${e.ecraEnviado} ↓${e.ecraRecebido}`).join(' | ');
      // O que interessa na câmara é o mesmo que interessa no ecrã: não "chegaram bytes",
      // mas "saiu imagem". `frames` conta o que o descodificador DESENHOU.
      const cams = [...camarasRecebidas.entries()]
        .map(([k, c]) => `${k.slice(0, 6)} ${c.frames} frames`).join(' | ');
      diz(`par ${volta}/6: ${gente.length} presente(s) ${resumo || '(sem ligações)'}`
        + ` | a ouvir ${voz.audio.size} | ${ecra || '—'}`
        + ` | câmaras: ${cams || 'nenhuma'} (anunciadas: ${voz.comCamara.size})`);

      // O anfitrião olha para o próprio ecrã A MEIO da transmissão — o cenário exato do
      // bug do ecrã preto: quem começa a ver tarde precisa do cabeçalho reenviado.
      if (modo === '' && volta === 2 && !voz.aVer) assistir(voz.eu);

      // Assim que alguém aparecer a transmitir, o convidado carrega em Assistir e conta o
      // que o <video> conseguiu mesmo descodificar — que é o que separa "chegaram bytes"
      // de "vê-se imagem".
      if (modo !== '' && !voz.aVer) {
        const quem = [...voz.aPartilhar][0];
        if (quem) {
          const t0 = performance.now();
          assistir(quem);
          diz(`par CONVIDADO a assistir a ${quem.slice(0, 6)}`);
          // Quanto tempo até APARECER imagem. É o número que o frame-chave fixo melhora:
          // sem ele, dependia da placa gráfica de quem partilha e não era determinável.
          (async () => {
            for (let i = 0; i < 300; i++) {
              const el = ecraDe(quem);
              const q = el && el.getVideoPlaybackQuality ? el.getVideoPlaybackQuality() : null;
              if (q && q.totalVideoFrames > 0) {
                return diz(`par PRIMEIRA IMAGEM em ${Math.round(performance.now() - t0)} ms`);
              }
              await esperar(100);
            }
            diz('par PRIMEIRA IMAGEM: nunca chegou em 30 s');
          })();
        }
      }
      if (voz.aVer) {
        const el = ecraDe(voz.aVer);
        const q = el && el.getVideoPlaybackQuality ? el.getVideoPlaybackQuality() : null;
        // O buffer distingue as duas avarias que dao o mesmo readyState=2: FOME (o buffer
        // acaba logo a seguir ao instante actual) e BURACO (ha dados a frente, mas com um
        // vazio pelo meio que o leitor nao salta). Sem isto, ficava-se a adivinhar.
        let faixas = '—';
        if (el && el.buffered) {
          faixas = [];
          for (let i = 0; i < el.buffered.length; i++) {
            faixas.push(`${el.buffered.start(i).toFixed(2)}-${el.buffered.end(i).toFixed(2)}`);
          }
          faixas = faixas.join(' , ') || 'vazio';
        }
        diz(`par imagem: ${el ? `${el.videoWidth}x${el.videoHeight}` : 'sem <video>'}`
          + ` readyState=${el ? el.readyState : '-'}`
          + ` frames=${q ? q.totalVideoFrames : '?'}`
          + ` t=${el ? el.currentTime.toFixed(2) : '-'}`
          + ` buffer=[${faixas}]`
          + ` codec=${el && el.__codec ? el.__codec : '?'}`
          + ` erro=${el && el.error ? el.error.code : 'nenhum'}`);
      }
    }
  } catch (e) {
    diz(`par FALHOU: ${e}`);
  }
})();

/* ---------- a barra da janela é nossa -------------------------------------- */

/* A moldura do Windows saiu; estes botões e o arrasto tomam-lhe o lugar. O
   `-webkit-app-region: drag` do Chromium não funciona na WebView2 — o arrasto tem de ser
   pedido ao Tauri no mousedown, e é por isso que isto é JavaScript e não três linhas de
   CSS. */
(function barraDaJanela() {
  if (!window.__TAURI__) return;
  const janela = window.__TAURI__.window.getCurrentWindow();

  document.addEventListener('mousedown', ev => {
    if (ev.button !== 0) return;
    const barra = ev.target.closest('.topbar, .barra-instalador, .janela__botoes');
    if (!barra) return;
    // Um clique num botão, campo ou chip é para esse elemento, não para arrastar.
    if (ev.target.closest('button, input, textarea, select, a, .chip, [role="button"]')) return;
    janela.startDragging();
  });

  document.addEventListener('dblclick', ev => {
    const barra = ev.target.closest('.topbar, .barra-instalador');
    if (!barra || ev.target.closest('button, input, .chip, [role="button"]')) return;
    janela.toggleMaximize();
  });

  document.addEventListener('click', ev => {
    const bt = ev.target.closest('[data-janela]');
    if (!bt) return;
    ev.stopPropagation();
    const accao = bt.dataset.janela;
    if (accao === 'minimizar') janela.minimize();
    else if (accao === 'maximizar') janela.toggleMaximize();
    else if (accao === 'fechar') janela.close();
  });
})();

/* ---------- medir a interface, para se poder verificar sem olhos ----------- */

(async () => {
  if (!window.__TAURI__) return;
  if (!(await invoke('medir_ui_pedido').catch(() => false))) return;
  const diz = linha => invoke('capacidades', { linha }).catch(() => {});
  await new Promise(r => setTimeout(r, 1200));

  const medir = sel => {
    const el = document.querySelector(sel);
    if (!el) return `${sel}: NAO EXISTE`;
    const r = el.getBoundingClientRect();
    const est = getComputedStyle(el);
    return `${sel}: ${Math.round(r.left)},${Math.round(r.top)} ${Math.round(r.width)}x${Math.round(r.height)}`
      + ` vis=${est.visibility} disp=${est.display} op=${est.opacity} z=${est.zIndex}`;
  };
  diz(`ui janela=${innerWidth}x${innerHeight}`);
  for (const s of ['.janela__botoes', '.janela__bt', '.janela__bt--fechar', '.topbar', '.transport', '.members']) {
    diz('ui ' + medir(s));
  }
  const bt = document.querySelector('.janela__bt--fechar');
  if (bt) {
    const r = bt.getBoundingClientRect();
    const emCima = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
    diz(`ui quem esta no ponto do fechar: ${emCima ? emCima.className || emCima.tagName : 'nada'}`);
  }

  // E o seletor de fontes, que só existe aberto: abre-se, espera-se pelas miniaturas,
  // mede-se a grelha e fecha-se. Foi um erro de cascata aqui (caixa a 420px, uma coluna)
  // que só um screenshot do dono apanhou — nunca mais sem medição.
  try {
    await escolherFonte();
  } catch (e) {
    diz(`ui seletor REBENTOU ao abrir: ${e && e.message ? e.message : e}`);
  }
  await new Promise(r => setTimeout(r, 3500));
  const caixaF = document.querySelector('.caixa--fontes');
  const grelha = document.querySelector('.fontes');
  const cartoes = document.querySelectorAll('.fonte');
  if (caixaF && grelha) {
    const colunas = getComputedStyle(grelha).gridTemplateColumns.split(' ').length;
    const c1 = cartoes[0] ? cartoes[0].getBoundingClientRect() : null;
    diz(`ui seletor: caixa=${Math.round(caixaF.getBoundingClientRect().width)}px`
      + ` colunas=${colunas} cartoes=${cartoes.length}`
      + (c1 ? ` primeiro=${Math.round(c1.width)}x${Math.round(c1.height)}` : ''));
    // E os dois separadores, um a um: cada um só pode mostrar o seu tipo.
    for (const aba of document.querySelectorAll('#abas-fontes .aba')) {
      aba.click();
      const n = document.querySelectorAll('.fonte').length;
      diz(`ui aba ${aba.dataset.aba}: ${n} cartoes`);
    }
    // A engrenagem abre o MENU do modo de transmissão; percorre-se tudo.
    // Estado conhecido primeiro: uma pessoa a mexer na janela durante a medição (já
    // aconteceu) abre e fecha o menu debaixo dos números. Mede-se do fechado.
    const menu = $('#menu-transmissao');
    menu.hidden = true;
    $('#btn-qualidade').click();
    const r = menu.getBoundingClientRect();
    diz(`ui menu: aberto=${!menu.hidden} ${Math.round(r.width)}x${Math.round(r.height)}`
      + ` modos=${document.querySelectorAll('[data-modo]').length}`
      + ` resumo="${$('#resumo-qualidade').textContent}"`);
    for (const nome of ['altura', 'fps', 'debito']) {
      const sub = $('#sub-' + nome);
      document.querySelector(`[data-abre="${nome}"]`).click();
      const aberto = !sub.hidden && sub.getBoundingClientRect().height > 0;
      document.querySelector(`[data-abre="${nome}"]`).click();
      // O atributo hidden não chega: o bug dos submenus eternos era o display:flex a
      // vencê-lo. Mede-se a ALTURA REAL, que é o que os olhos veem.
      const fechado = sub.getBoundingClientRect().height === 0;
      diz(`ui sub ${nome}: ${document.querySelectorAll('#sub-' + nome + ' .menu-trans__opcao').length} opções`
        + ` abre=${aberto} fecha=${fechado}`);
    }
    // ---- o palco da transmissão ------------------------------------------------
    //
    // Mede-se com estado FABRICADO: finge-se uma sala com duas pessoas, uma a transmitir,
    // e conta-se o que a vista produz. É a única forma de exercitar aqui um palco que, na
    // vida real, precisa de duas máquinas ligadas.
    {
      const antes = {
        eu: voz.eu, canal: voz.canal, servidor: voz.servidor, ecra: voz.ecra,
        aVer: voz.aVer, tam: voz.ecraTamanho, srv: servidorAtual, cnl: canalAtual,
        qual: voz.qualidadeEmUso,
      };
      // Uma sala de voz de verdade, criada para a medição — a vista olha para o canal
      // SELECIONADO, e sem um canal de voz escolhido ela nem chega a desenhar.
      let srv = vista.servidores.find(x => x.canais.some(c => c.tipo === 'voz'));
      if (!srv) {
        const id = await invoke('criar_servidor', { nome: 'medicao' });
        await invoke('criar_canal', { servidor: id, nome: 'palco', tipo: 'voz' });
        await desenharTudo();
        srv = vista.servidores.find(x => x.id === id);
      }
      const cv = srv.canais.find(c => c.tipo === 'voz');
      servidorAtual = srv.id; canalAtual = cv.id;
      voz.eu = 'eueueu';
      voz.canal = cv.id;
      voz.servidor = srv.id;
      voz.presentes.set('outro1', voz.canal);
      voz.ecra = { fechar() {} };
      voz.ecraTamanho = { largura: 2560, altura: 1440 };
      voz.aSerVistoPor.add('outro1');
      voz.aPartilhar.add('outro1');
      voz.infoDaTransmissao.set('outro1', { qualidade: '1080p 30FPS', espectadores: 3 });
      voz.aVer = voz.eu;
      // Os três casos do rótulo, com estado fabricado: a fonte mais pequena que o pedido
      // (o caso do dono), do tamanho certo, e "Nativa".
      for (const [pedida, real, nome] of [[1440, 1050, 'fonte menor'],
                                          [1440, 1440, 'igual'],
                                          [0, 1200, 'nativa']]) {
        voz.qualidadeEmUso = { altura: pedida, fps: 60, debito: 0, som: true };
        voz.ecraTamanho = { largura: Math.round(real * 16 / 9), altura: real };
        diz(`ui rotulo (${nome}): "${rotuloDaQualidade()}" | ${porqueEstaResolucao()}`);
      }
      voz.qualidadeEmUso = { altura: 1440, fps: 60, debito: 0, som: true };
      gentePorBaixoOculta = false;
      janelaComFoco = true;
      desenharVoz();

      const palco = document.querySelector('.palco');
      const selos = [...document.querySelectorAll('.palco__selo')].map(e => e.textContent.trim());
      const bts = document.querySelectorAll('.palco__meio .palco__bt');
      diz('ui palco: existe=' + !!palco
        + ' selos=' + JSON.stringify(selos)
        + ' botoes=' + bts.length
        + ' onde="' + ($('.palco__onde') ? $('.palco__onde').textContent.trim() : '') + '"'
        + ' convidar=' + !!document.querySelector('.palco__baixo-esq .palco__bt')
        + ' fotinhas=' + document.querySelectorAll('.mini').length
        + ' aoVivoNasFotinhas=' + document.querySelectorAll('.mini__vivo').length
        + ' tampasDeAssistir=' + document.querySelectorAll('.mini__tampa').length);

      // O botão do meio esconde as fotinhas, e volta a mostrá-las.
      const antesOculto = document.querySelector('.palco__gente').hidden;
      [...bts].find(b => /Ocultar/.test(b.title)).click();
      const depoisOculto = document.querySelector('.palco__gente').hidden;
      [...document.querySelectorAll('.palco__meio .palco__bt')]
        .find(b => /Mostrar/.test(b.title)).click();
      const reposto = document.querySelector('.palco__gente').hidden;
      diz('ui palco ocultar: antes=' + antesOculto + ' depois=' + depoisOculto
        + ' reposto=' + reposto);

      // Sem foco, aparece a mensagem de pausa — e SÓ para quem transmite.
      janelaComFoco = false;
      desenharVoz();
      const pausa = document.querySelector('.palco__pausa');
      diz('ui palco pausa: aparece=' + !!pausa
        + ' texto="' + (pausa ? pausa.textContent.replace(/\s+/g, ' ').trim().slice(0, 46) : '') + '"');
      janelaComFoco = true;

      // E o palco de OUTRA pessoa: sem botão de parar transmissão, com o nome dela.
      voz.aVer = 'outro1';
      desenharVoz();
      const bts2 = [...document.querySelectorAll('.palco__meio .palco__bt')].map(b => b.title);
      diz('ui palco alheio: botoes=' + JSON.stringify(bts2.map(t => t.split(' ')[0]))
        + ' qualidade="' + ([...document.querySelectorAll('.palco__selo')][0] || {}).textContent + '"'
        + ' semPausa=' + !document.querySelector('.palco__pausa'));

      voz.presentes.delete('outro1');
      voz.aSerVistoPor.delete('outro1');
      voz.aPartilhar.delete('outro1');
      voz.infoDaTransmissao.delete('outro1');
      Object.assign(voz, { eu: antes.eu, canal: antes.canal, servidor: antes.servidor,
        ecra: antes.ecra, aVer: antes.aVer, ecraTamanho: antes.tam,
        qualidadeEmUso: antes.qual });
      servidorAtual = antes.srv; canalAtual = antes.cnl;
      desenharVoz();
    }

    // ---- as avarias que antes eram invisiveis ---------------------------------
    {
      const antes = { eu: voz.eu, canal: voz.canal, srv: servidorAtual, cnl: canalAtual };
      let srv = vista.servidores.find(x => x.canais.some(c => c.tipo === 'voz'));
      if (!srv) {
        const id = await invoke('criar_servidor', { nome: 'medicao' });
        await invoke('criar_canal', { servidor: id, nome: 'palco', tipo: 'voz' });
        await desenharTudo();
        srv = vista.servidores.find(x => x.id === id);
      }
      const cv = srv.canais.find(c => c.tipo === 'voz');
      servidorAtual = srv.id; canalAtual = cv.id;
      voz.eu = 'eueueu'; voz.canal = cv.id; voz.servidor = srv.id;
      voz.presentes.set('outro1', cv.id);

      // 1) a minha voz morreu: o botao do microfone tem de dizer PORQUE.
      vozFalhou = 'O codificador de voz desistiu — ninguém te ouve.';
      desenharRodape(); await new Promise(r => setTimeout(r, 250));
      diz(`ui voz morta: cortado=${$('#btn-mic').classList.contains('is-cortado')}`
        + ` title-diz-porque=${/desistiu/.test($('#btn-mic').title)}`);
      vozFalhou = null;

      // 2) deixei de ouvir UMA pessoa: ela tem de ficar marcada.
      vozPartida.set('outro1', 'o áudio desta pessoa não está a descodificar');
      voz.aVer = null; desenharVoz();
      const marcas = document.querySelectorAll('.tile__sem-audio');
      diz(`ui voz de um so: marcas=${marcas.length}`
        + ` no-painel-certo=${!!document.querySelector('.tile[data-chave="outro1"] .tile__sem-audio')}`);
      vozPartida.clear();

      // 3) o codec que esta maquina nao le: em vez de preto, a razao.
      const falso = { porqueNaoDa: () => 'Esta máquina não sabe descodificar avc1.640033.' };
      fluxosRecebidos.set('outro1', falso);
      voz.aPartilhar.add('outro1'); voz.aVer = 'outro1';
      desenharVoz();
      const pausa = document.querySelector('.palco__pausa');
      diz(`ui codec recusado: explica=${!!pausa}`
        + ` texto="${pausa ? pausa.textContent.replace(/\s+/g, ' ').slice(0, 46) : ''}"`);

      fluxosRecebidos.delete('outro1'); voz.aPartilhar.delete('outro1');
      voz.presentes.delete('outro1'); voz.aVer = null;
      Object.assign(voz, { eu: antes.eu, canal: antes.canal });
      servidorAtual = antes.srv; canalAtual = antes.cnl;
      desenharVoz();
    }

    // ---- as Definicoes, todas as seccoes --------------------------------------
    //
    // Nao se mede so "abriu": desenha-se CADA seccao e exige-se que cada uma produza um
    // titulo e conteudo. Uma seccao que rebenta a desenhar deixaria um painel em branco, e
    // um painel em branco parece uma definicao que ainda nao foi feita -- exactamente o
    // que este painel tem de saber distinguir.
    {
      await abrirDefinicoes();
      const lado = $('#defs');
      const itens = [...document.querySelectorAll('.defs__item')].map(b => b.textContent.trim());
      const grupos = [...document.querySelectorAll('.defs__grupo')].map(b => b.textContent.trim());
      diz(`ui defs: aberto=${!lado.hidden} seccoes=${itens.length} grupos=${JSON.stringify(grupos)}`
        + ` avatar=${!!$('#defs-avatar').style.backgroundImage}`
        + ` busca=${!!$('#defs-buscar')}`);

      // "Editar perfil" tem de LEVAR a algum lado, e o nome tem de mudar mesmo. Verificar
      // que o botão existe não distingue um atalho de um enfeite.
      await mostrarPainel('sistema');
      $('#defs-editar').click();
      await new Promise(r => setTimeout(r, 120));
      const foiParaConta = painelActivo === 'conta' && !!$('#def-nome');
      const antigo = vista.nome || '';
      $('#def-nome').value = 'medicao-' + antigo;
      [...document.querySelectorAll('#defs-painel .btn--primary')]
        .find(b => b.textContent === 'Guardar').click();
      await new Promise(r => setTimeout(r, 500));
      const mudou = vista.nome === 'medicao-' + antigo;
      $('#def-nome').value = antigo;
      [...document.querySelectorAll('#defs-painel .btn--primary')]
        .find(b => b.textContent === 'Guardar').click();
      await new Promise(r => setTimeout(r, 500));
      diz(`ui defs perfil: editar-leva-a-conta=${foiParaConta} nome-mudou=${mudou}`
        + ` reposto=${vista.nome === antigo} sidebar="${$('#defs-nome').textContent}"`);

      let falharam = [];
      let vazias = 0;
      for (const chave of ORDEM) {
        try {
          await mostrarPainel(chave);
          const painel = $('#defs-painel');
          const titulo = painel.querySelector('h2');
          const conteudo = painel.textContent.trim().length;
          if (!titulo || conteudo < 60) falharam.push(chave);
          if (PAINEIS[chave].vazia) {
            // As vazias TEM de dizer que estao vazias -- e nao mostrar um painel mudo.
            const diz_o = /ainda não existe|não vai existir/i.test(painel.textContent);
            if (!diz_o) falharam.push(chave + ':nao-avisa');
            vazias += 1;
          }
        } catch (e) {
          falharam.push(`${chave}:${e && e.message ? e.message : e}`);
        }
      }
      diz(`ui defs paineis: ${ORDEM.length} desenhados, ${vazias} honestamente vazios,`
        + ` falharam=${JSON.stringify(falharam)}`);

      // A busca filtra, e o que nao existe diz-se.
      $('#defs-buscar').value = 'voz';
      desenharMenuDeDefinicoes('voz');
      const comVoz = document.querySelectorAll('.defs__item').length;
      $('#defs-buscar').value = 'xpto';
      desenharMenuDeDefinicoes('xpto');
      const semNada = !!document.querySelector('.defs__nada');
      $('#defs-buscar').value = '';
      desenharMenuDeDefinicoes('');
      diz(`ui defs busca: "voz"=${comVoz} item(ns), "xpto" diz-que-nao-ha=${semNada},`
        + ` limpa=${document.querySelectorAll('.defs__item').length}`);

      // As 24 palavras vivem na Conta, que e onde alguem as iria procurar.
      await mostrarPainel('conta');
      const ver = [...document.querySelectorAll('#defs-painel .btn')]
        .find(b => /Mostrar as palavras/.test(b.textContent));
      ver.click();
      await new Promise(r => setTimeout(r, 400));
      const ps = [...document.querySelectorAll('#defs-painel .palavras span')]
        .map(l => l.textContent.replace(/^\s*\d+\s*/, ''));
      diz(`ui defs palavras: ${ps.length} mostradas, distintas=${new Set(ps).size},`
        + ` vazias=${ps.filter(x => !x.trim()).length}, botao-escondido=${ver.hidden}`);

      // Uma palavra trocada NAO pode restaurar. Verifica-se o resultado, nao a redaccao.
      [...document.querySelectorAll('#defs-painel .btn')]
        .find(b => /Restaurar de outras/.test(b.textContent)).click();
      $('#palavras-entrada').value = ps.slice(0, 23).join(' ') + ' zebra';
      [...document.querySelectorAll('#defs-painel .btn--perigo')][0].click();
      await new Promise(r => setTimeout(r, 500));
      const msg = $('#restaurar-nota').textContent;
      diz(`ui defs restauro: recusado=${!/restaurada/i.test(msg) && msg.length > 0}`
        + ` msg="${msg.slice(0, 46)}"`);
      $('#palavras-entrada').value = '';

      // O interruptor da privacidade tem de CALAR a pergunta, não esconder a resposta.
      await mostrarPainel('dados');
      const alvo = [...document.querySelectorAll('#defs-painel label.def__linha')]
        .find(l => /não olhar/i.test(l.textContent))
        .querySelector('input[type=checkbox]');
      const ligado = () => localStorage.getItem(SEM_JOGO) === '1';
      if (ligado()) alvo.click();                  // garantir que começa ligada
      await new Promise(r => setTimeout(r, 60));
      const antes = perguntasSobreJanelas;
      await verJogo();
      const comDeteccao = perguntasSobreJanelas - antes;
      alvo.click();                                 // desligar a deteção
      await new Promise(r => setTimeout(r, 60));
      const meio = perguntasSobreJanelas;
      await verJogo();
      const semDeteccao = perguntasSobreJanelas - meio;
      const rs = ['ha', 'nao', 'falhou', undefined].map(respostaDaProcura);
      diz(`ui defs update: distintas=${new Set(rs.slice(0, 3)).size}/3`
        + ` falhou-nao-mente=${rs[2] !== rs[1] && /não consegui/.test(rs[2])}`
        + ` desconhecido-cai-no-seguro=${rs[3] === rs[2]}`);

      diz(`ui defs janelas: guardada=${ligado()} perguntas com=${comDeteccao} sem=${semDeteccao}`
        + ` cartao-escondido=${$('#jogo').hidden} jogo-esquecido=${jogoAberto === null}`);
      if (ligado()) alvo.click();                   // deixar como estava
      fecharDefinicoes();
    }


    const rotuloAudio = $('#linha-som').querySelector('b').getBoundingClientRect();
    const antes = $('#mudo-transmissao').checked;
    $('#linha-som').click();
    const depois = $('#mudo-transmissao').checked;
    $('#linha-som').click();
    diz(`ui silenciar: rotulo ${Math.round(rotuloAudio.width)}x${Math.round(rotuloAudio.height)}`
      + ` (uma linha se altura < 22) alterna=${antes !== depois}`
      + ` reposto=${$('#mudo-transmissao').checked === antes}`);
    document.querySelector('[data-modo="jogos"]').click();
    diz(`ui modo jogos: resumo="${$('#resumo-qualidade').textContent}"`);
    document.querySelector('[data-modo="pers"]').click();
    $('#btn-qualidade').click();
  } else {
    diz('ui seletor: NAO ABRIU');
  }
  fechar('veu-fontes');
})();
