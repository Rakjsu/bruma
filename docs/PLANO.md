# Bruma — um Discord P2P, anónimo e E2EE, sem servidor

> Nome escolhido: **Bruma** (névoa, em PT e ES; pronunciável em inglês). Domínio livre confirmado por
> RDAP: `bruma.chat`. **Falta verificar marca registada** — passo separado, antes de qualquer uso público.

## Contexto

O objetivo é um Discord próprio — servidores, canais, chat, voz, vídeo e **partilha de ecrã** — para o
dono e os amigos, com hipótese de virar produto público. O modelo de confiança pedido é o oposto do
Discord real:

- **Ninguém precisa de nada para entrar.** Sem e-mail, telefone ou password. A identidade é um par de
  chaves Ed25519 gerado no dispositivo; 12 palavras recuperam a conta noutro PC.
- **Não há servidor.** Não há máquina central a guardar mensagens, nem sequer cifradas. Cada membro
  online guarda o registo cifrado do canal e sincroniza-o com quem chegar — é isto que faz com que
  **nada morra por estares offline**, sem existir arquivo em lado nenhum.
- **Anónimo por omissão.** O ID é uma chave pública, não um nome. O tráfego de chat passa por um relay
  que reencaminha bytes cifrados que não consegue ler.

Entrega-se em **desktop (Tauri 2)** primeiro; web e mobile seguem depois, e a arquitetura é escolhida
para que isso não obrigue a reescrever o núcleo.

## Decisões fechadas

| Decisão | Escolha | Porquê |
|---|---|---|
| Rede | **`iroh` 1.0.3 (QUIC P2P)**, sem servidor | Liga-se **por chave pública, não por IP** — é literalmente a identidade Ed25519 que já usamos. ~90% de hole-punch direto, relay como fallback, wire protocol estável, bindings oficiais Node/Swift/Kotlin (cobre web e mobile depois). |
| Relay | **Relay público do n0** | Zero infraestrutura própria. Escotilha de fuga: `iroh-relay` é o mesmo crate, self-hostável depois sem mexer no cliente. |
| Mensagens | **Log assinado encadeado por hash**, não CRDT | Um log append-only ordenado por (timestamp, hash) chega e é trivial de sincronizar. Poupa uma dependência pesada e muita superfície de bugs. |
| Estado mutável | **CRDT (`loro` 1.13)** só para canais, cargos, membros, reações e edições | É aqui que há fusão a sério. O log de mensagens não precisa disto. |
| Identidade | Ed25519 no dispositivo + mnemónica BIP39 de 12 palavras | Zero PII. A mesma chave é o `NodeId` do iroh. |
| Cripto de grupo | **Sender keys** por época (Fase 1) → **MLS/OpenMLS 0.8.1** (Fase 3) | Chat E2EE de pé em dias; troca isolada atrás de um trait `GroupKeyAgreement`. |
| Média | **WebRTC mesh** (≤6) + **SFU LiveKit opcional** | Mesh não precisa de infra; o SFU é a opção para canais maiores *e* para esconder o IP na chamada. |
| Moderação | **Expulsão por rotação de chave de época** | Sem autoridade central, a única garantia real é matemática: o expulso deixa de conseguir decifrar. |
| Anexos | **`iroh-blobs`** (BLAKE3, content-addressed) | Transferência P2P resumível e verificada, cifrada antes de sair do cliente. |
| BD local | SQLite + SQLCipher (`rusqlite`) | O histórico real vive aqui, cifrado com chave derivada da identidade. |

## Arquitetura

Monorepo pnpm + Turborepo na raiz do repositorio. **Não há `apps/server`.**

```
apps/
  desktop/          Tauri 2 + React 19 + Vite + TS
crates/
  bruma-node/       endpoint iroh, gossip, sync do log e do CRDT, transporte (QUIC | Tor)
  bruma-crypto/     identidade, épocas, envelope, anexos → nativo + wasm32 (web futura)
  bruma-store/      SQLCipher: log materializado, fila de envio, FTS5
  bruma-media/      signaling mesh sobre iroh; árvore de distribuição; adaptador LiveKit
packages/
  protocol/         tipos TS + schemas zod + eventos + resolver de permissões
infra/              OPCIONAL: livekit + coturn, e iroh-relay próprio, para quem quiser
```

**Identidade e transporte.** A chave Ed25519 é gerada no arranque, selada no keychain do SO
(`keyring` 4.1) e recuperável por 12 palavras BIP39. É simultaneamente o ID de utilizador e o `NodeId`
do iroh — não existe passo de "login". **As chaves privadas nunca entram no JavaScript**: a UI chama
comandos Tauri e todo o material criptográfico fica em Rust. **Usar só primitivas auditadas**
(RustCrypto / dalek); nunca inventar construções.

**Descoberta.** Não há DHT global nem diretório público. Um convite é um link que carrega os NodeIds de
bootstrap, o id do canal e uma chave de convite assinada. Quem não tem o link não sabe que o canal existe.

**Dois planos de estado, e a razão é criptográfica.** Um CRDT não consegue fundir aquilo que não
consegue ler — isto tem de ficar decidido antes de escrever código, ou reescreve-se a camada de sync:
- **Log de mensagens**: append-only, payload **opaco de ponta a ponta**. Cada entrada é assinada e
  encadeada por hash à anterior; ordena por (timestamp, hash). Não precisa de fusão semântica.
- **Documento de configuração da guild** (canais, cargos, membros, reações, edições): **desencriptado
  localmente, fundido em claro em memória pelo Loro, e re-encriptado para transporte.** Nunca sai em claro.

**Cripto de grupo.** Cada canal tem uma chave simétrica de época (XChaCha20-Poly1305). Ao entrar, um
membro recebe-a selada para a sua prekey X25519. Cada mensagem leva nonce próprio e assinatura. Kick ou
saída **rodam a época** e a nova chave é selada só para quem fica. Tudo atrás do trait
`GroupKeyAgreement`, para o MLS entrar na Fase 3 sem tocar no pipeline de mensagens. **O bug provável
não está na cifra, está na rotação de época** — testes de propriedade para isso desde o primeiro dia.

**Permissões.** Bitfield estilo Discord resolvido por uma função pura em `packages/protocol`, usada por
todos os clientes. Sem servidor, a autorização é por assinatura: operações de admin são válidas se
assinadas por uma chave com o cargo certo, e a única imposição forte é a rotação de época.

**Média.** Signaling de WebRTC por cima do iroh (já temos canal autenticado). Mesh direto entre
participantes. O adaptador LiveKit fica atrás da mesma interface em `bruma-media`.

## Modo Fantasma (botão com explicação na UI)

Um toggle visível que troca o transporte de chat de QUIC/iroh para **onion services** — cada peer publica
um `.onion` próprio com `tor-hsservice` 0.45 / `arti-client` 0.45 embutidos no Rust. Sem daemon externo,
sem configuração e **sem abrir portas no router**.

O botão tem de dizer, em texto simples, exatamente isto:
- ✅ Ninguém — nem os outros membros, nem o relay — fica a saber o teu IP.
- ⚠️ O chat fica mais lento (~200–800 ms por mensagem).
- ❌ **Voz, vídeo e partilha de ecrã ficam indisponíveis.** Não é uma limitação da app: o Tor só
  transporta TCP e a proposta 348 (UDP) nunca foi implantada, por isso WebRTC não funciona por Tor.

Regra de copy para toda a UI: **nunca prometer "anónimo" em abstrato.** Dizer exatamente o que faz e o
que não faz. Prometer demais em privacidade é como estes projetos morrem publicamente.

## Passo 0 — Higiene do repositório

A pasta ainda não é um repositório git. Antes de qualquer código: `git init`, `.gitignore` (Rust, Node,
Tauri, `*.db`, `*.key`), e commits pequenos desde o primeiro spike.

## Fase 0 — Spikes de risco, por ordem de quanto matam o projeto

Descartáveis, vivem em `spikes/`. Cada um tem um gate. **A ordem importa**: o Spike 1 é mais fundamental
e mais barato que o 2 — se o P2P não aguentar, cai a arquitetura inteira e volta-se a um desenho com
servidor, o que invalida tudo o resto. Se a partilha de ecrã falhar, só se troca o caminho de captura.

**Spike 1 — a premissa serverless (o que mais mata o projeto).**
Duas instâncias **em redes diferentes** — o PC de casa e o de um amigo noutra casa — ligam-se só pela
chave pública via relay público do n0, trocam uma mensagem cifrada; uma fecha, a outra escreve, e ao
voltar sincroniza o que perdeu.
- **Duas VMs em casa não provam nada** — partilham o mesmo NAT.
- **Gate**: CGNAT é comum em fibra residencial e móvel em PT e no BR. Se o hole-punch falhar e tudo cair
  no relay público, o projeto precisa de relay próprio — decisão a tomar aqui, não no terceiro mês.

**Spike 2 — partilha de ecrã no Tauri.**
Partilhar ecrã com áudio do sistema entre dois clientes.
- Caminho A: `getDisplayMedia()` na webview, com `PermissionKind::DisplayCapture` no wry ≥ 0.56.
  Verificar picker de fontes, áudio do sistema e cursor. **Medir o bitrate real** com `contentHint='text'`
  vs sem, e se o AV1 usa hardware (NVENC) ou cai em libaom por software.
- Caminho B: captura nativa em Rust (`scap` 0.0.8) publicada pelo crate `livekit` 0.8.3.
- **Gate**: se nenhum der ecrã+áudio fiável no Windows, parar e reavaliar (Electron ou janela dedicada).

**Spike 3 — Modo Fantasma.** — **EXECUTADO, BLOQUEADO (21/08/2026)**
Dois peers a sincronizar chat por `.onion` com arti embutido, sem tor externo e sem portas abertas.
O arti embebe-se, liga-se a relays reais e recebe o consenso, mas o `create_bootstrapped()` nunca
retorna e nada é gravado em cache. Cinco causas eliminadas com medições (operador, lentidão,
relógio, permissões, runtime) — ver `spikes/spike3-ghost/README.md`. Suspeita-se de um problema do
arti no Windows. **Consequência para o plano:** o Modo Fantasma deixa de ser dado como adquirido na
Fase 2. Nada da Fase 1 depende dele.

## Fase 1 — MVP usável

Construção **por camadas completas** (cada crate terminado antes do seguinte), com uma salvaguarda:
**no fim de cada crate, um teste de integração contra o crate anterior**, para as peças se provarem umas
às outras sem esperar pela UI.

1. **Fundações** — monorepo, `packages/protocol` (eventos, zod, permissões), `bruma-node` com endpoint
   iroh + gossip, `bruma-store` com SQLCipher e FTS5.
2. **Identidade** — geração, ecrã de backup obrigatório das 12 palavras, keychain, restauro noutro PC,
   prekeys X25519 publicadas no documento da guild.
3. **Guilds e canais** — criar, convidar por link, canais de texto e voz, lista de membros, presença.
4. **Cargos e permissões** — bitfield, overwrites por canal, hierarquia, kick/ban por operação assinada.
5. **Chat E2EE** — épocas, selagem por prekey, rotação em kick/leave, log assinado encadeado por hash,
   fila de envio offline, histórico local cifrado e pesquisa local.
6. **Voz, vídeo e ecrã** — mesh WebRTC, entrar/sair, mute/deafen, indicador de quem fala, partilha de
   ecrã com áudio, webcam. Inclui **camadas 1–3 e 5 do orçamento de upload** (contentHint/AV1 SCC,
   perfil por tipo de conteúdo, só enviar a quem está a ver, qualidade por espectador, teto de upload).
   **Aviso explícito na UI de que numa chamada mesh os participantes veem o teu IP.**
7. **Anexos e média rica** — `iroh-blobs` cifrado, previews, emojis personalizados, reações, respostas,
   editar/apagar, markdown.
8. **Endurecimento base (entra já, é barato e difícil de acrescentar depois)** — zero telemetria, zero
   CDN e zero fontes remotas, sem User-Agent identificável nem device ID, padding de mensagens e anexos
   em escalões fixos (256B/1K/4K/16K), DoH para resolução de nomes, SQLCipher com chave derivada da
   identidade.
9. **Acabamentos** — notificações nativas, typing indicators, estados de leitura, reconexão e resync.

**Expectativa de tempo, sem floreados**: a Fase 1 assim especificada é trabalho de meses para uma pessoa.
Cada um destes nove pontos é um projeto pequeno.

## Fases seguintes

- **Fase 2 — desktop premium e anonimato avançado**: **Modo Fantasma** (Spike 3 em produto), **árvore de
  distribuição da partilha de ecrã** (camada 4 do orçamento de upload, com escolha de reencaminhadores
  por capacidade medida e reparação da árvore), SFU LiveKit opcional por canal, DMs e grupos,
  amigos/bloqueio, overlay in-game, push-to-talk global, hotkeys, supressão de ruído no cliente,
  threads, pins, menções, auto-update assinado do Tauri.
- **Fase 3 — segurança forte e escala**: substituir sender keys por **MLS (OpenMLS)**, sealed sender,
  verificação de identidade por QR/safety numbers, relay `iroh-relay` próprio como opção, cliente **web**
  (`bruma-crypto` para WASM, bindings Node do iroh) e **mobile** (bindings Swift/Kotlin do iroh).
- **Fase 4 — extras**: stage channels, soundboard, stickers, canais de fórum, bots/webhooks.

## Orçamento de upload da partilha de ecrã

O problema não é o ecrã ser pesado — é enviarmos a mesma coisa N vezes e enviarmos pixels que não
mudaram. Cinco camadas, por ordem de retorno sobre esforço:

**1. Não codificar o que não mudou (grátis, maior ganho).** Um ecrã de código está ~95% estático.
- `contentHint = 'text'` / `'detail'` na track liga as ferramentas de *screen content coding* do AV1
  (palette mode + intra block copy) no Chromium — logo, funciona dentro da WebView2. IntraBC sozinho
  vale ~27% em conteúdo de ecrã; medições em slides dão 25%+ de redução de bitrate.
- `degradationPreference: 'maintain-resolution'` para texto (baixa fps, mantém nitidez) e
  `'maintain-framerate'` para jogo/vídeo. Trocar de perfil por deteção de movimento.
- No caminho nativo: `IDXGIOutputDuplication::GetFrameDirtyRects` + `GetFrameMoveRects` — o Windows diz
  que retângulos mudaram e quais foram movidos. Codificar só isso. Ecrã parado ≈ bitrate quase nulo.
  **Ordem obrigatória: processar move rects antes dos dirty rects.**

**2. Não enviar a quem não está a ver (grátis, corta o N).** Seis pessoas no canal mas só duas com a
janela aberta → duas streams. Pausar quando o espectador minimiza ou muda de separador, com um flag
"estou a ver" propagado por iroh. Na prática transforma ×5 em ×1–2 na maioria das sessões.

**3. Qualidade diferente por espectador (vantagem do mesh).** Em mesh cada ligação é independente:
1080p para quem tem em foco, 540p para quem tem a espreitar num canto, via `scaleResolutionDownBy`
por sender. Num SFU isto exigiria simulcast; em mesh sai de graça.

**4. Árvore de distribuição em vez de mesh (a correção estrutural).** Em vez de quem partilha enviar
N cópias, envia 1–2 e esses reencaminham. Cada nó carrega no máximo ~2 cópias e o upload de quem
partilha deixa de depender do tamanho do grupo — multicast ao nível da aplicação.
- Escolher os reencaminhadores pela capacidade de upload medida (as estatísticas de ligação do iroh
  dão isso), e reparar a árvore quando alguém sai.
- Custo: +50–150 ms por salto. Irrelevante para partilha de ecrã (não é jogo competitivo).
- **Não quebra o modelo de privacidade**: os frames vão cifrados ponta a ponta, por isso quem
  reencaminha está a mover bytes que não consegue descodificar.

**5. Reduzir a fonte e nunca exceder o upload real.** Partilhar uma janela em vez do monitor todo,
limitar a 1080p mesmo em ecrãs 4K, e medir a capacidade real de upload para impor um teto global
repartido pelos destinos — degradar qualidade (ou reduzir filhos diretos na árvore) em vez de deixar
a chamada quebrar e afogar a ligação de casa.

**Números realistas (1080p):**

| Cenário | Ingénuo (VP8, frames completos) | Com camadas 1–3 | Com árvore (camada 4) |
|---|---|---|---|
| Código/browser, 5 espectadores | ~35 Mbps | ~2–7 Mbps | **~1–2 Mbps** |
| Jogo/vídeo 60fps, 5 espectadores | ~35 Mbps | ~15–25 Mbps | **~4–6 Mbps** |

**Ressalva sobre AV1**: a RTX 5080 do dono tem AV1 NVENC, mas o AV1 do WebRTC no Chromium é por
software (libaom) e a 1080p60 é pesado em CPU. Amigos com GPUs mais antigas precisam de fallback para
H.264/VP9. **Confirmar no Spike 2 se o caminho de hardware fica acessível** — se não ficar, é mais um
argumento para o caminho nativo em Rust.

## Limitações que o desenho assume

1. **Numa chamada mesh os participantes veem o teu IP.** O WebRTC faz o seu próprio NAT traversal e o
   relay do iroh não o cobre. Esconder isso exige TURN ou o SFU opcional — é uma escolha por canal, com
   aviso na UI, não um problema resolvido por omissão.
2. **O relay público do n0 vê metadados** — que NodeIds falam entre si, quando e quanto volume. Nunca vê
   conteúdo. Mitigação disponível a qualquer momento: alojar o próprio `iroh-relay`.
3. **Sender keys não dão forward secrecy dentro de uma época.** Um dispositivo comprometido lê as
   mensagens dessa época. MLS resolve na Fase 3 — daí o trait desde o primeiro dia.
4. **"Ban" é criptográfico, não imposto.** O expulso deixa de decifrar o que vier a seguir, garantido.
   Mas continua a ter o que já tinha, e um cliente modificado pode ignorar a revogação para ler o
   histórico local dele.
5. **Se ninguém do canal estiver online, não há sincronização.** É a contrapartida direta de não haver
   servidor: o histórico existe em quem o tiver.
6. **O upload da partilha de ecrã cresce com o número de espectadores** — mitigado pelas cinco camadas
   acima, mas nunca eliminado. Quem tiver upload fraco (ADSL, 4G) continua a não conseguir partilhar
   1080p60 de jogo, por muito que se otimize.
7. **Privacy Pass ficou sem propósito com o desenho serverless** — não há a quem provar pertença; isso
   passa a ser o convite assinado e a posse da chave. Fica registado como condicional, para o caso de
   entrar um SFU ou relay próprio que precise de autorizar acesso sem identificar quem.
8. **E2EE mata funcionalidades de servidor**: sem gravação, transcoding, pesquisa remota ou previews
   gerados fora do cliente. E bots teriam de ser membros com a chave do canal (Fase 4, com aviso na UI).
9. **Marca**: não usar o nome, o logo, os sons nem os emojis do Discord — são licenciados e a marca é
   registada. E confirmar que "Bruma" está livre como marca antes de qualquer uso público.

## Ficheiros críticos a criar

- `crates/bruma-node/src/{endpoint,gossip,log,sync,transport}.rs` — iroh; `log` é o log assinado
  encadeado por hash; `transport` abstrai QUIC vs onion
- `crates/bruma-crypto/src/{identity,group,envelope,attachment}.rs` — inclui trait `GroupKeyAgreement`
- `crates/bruma-store/src/lib.rs` — SQLCipher, log materializado, fila de envio, FTS5
- `crates/bruma-media/src/{mesh,tree,budget,livekit}.rs` — signaling sobre iroh, árvore de distribuição,
  teto de upload e perfis de codificação, adaptador SFU atrás da mesma interface
- `packages/protocol/src/permissions.ts` — bitfield e resolver puro
- `packages/protocol/src/ops.ts` — operações assinadas e regra de resolução de conflitos do CRDT
- `apps/desktop/src-tauri/src/commands.rs` — ponte cripto (chaves nunca chegam ao JS)
- `apps/desktop/src/features/{voice,chat,guild,ghost}/` — UI, incluindo o painel do Modo Fantasma

## Verificação

**Gates da Fase 0** — sem estes, não se avança:
1. Dois PCs em casas diferentes ligam-se só por chave pública; um fica offline, volta e resincroniza.
2. Dois clientes com ecrã + áudio do sistema a passar, com E2EE ativo e bitrate medido.
3. Chat a sincronizar por `.onion` sem tor externo e sem portas abertas.

**Fase 1 — testes manuais com dois perfis em máquinas e redes diferentes:**
- Enviar com o destinatário offline → ele liga e recebe. **Provar que não há servidor**: desligar tudo
  menos os dois peers e confirmar que continua a funcionar.
- Inspecionar o ficheiro SQLite com um leitor externo → tem de ser ilegível (SQLCipher).
- Capturar o tráfego com Wireshark → só QUIC cifrado; nenhum conteúdo em claro.
- Anexar um ficheiro → confirmar que o blob transferido é ciphertext e sem nome original.
- Expulsar um membro → confirmar que a época roda e que ele deixa de decifrar mensagens novas.
- Restaurar a identidade noutro PC só com as 12 palavras e recuperar acesso aos canais.

**Automatizado:**
- `cargo test -p bruma-crypto` — vetores de cifra, selagem de chave, e **testes de propriedade da
  rotação de época** (é aqui que vai estar o bug).
- `cargo test -p bruma-node` — sync do log e do CRDT entre nós simulados, incluindo partição de rede e
  reconvergência.
- Teste de integração no fim de cada crate, contra o crate anterior.
- `vitest` em `packages/protocol` — resolver de permissões (tabela de casos) e resolução de conflitos.
- `playwright` sobre a build web do cliente para os fluxos de UI.
- Gate de CI: sem `any` no protocolo, `cargo clippy -- -D warnings`.
