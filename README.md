# Bruma

Um Discord P2P, anónimo e cifrado ponta a ponta — **sem servidor**.

Sem e-mail, sem telefone, sem password. A tua identidade é um par de chaves gerado no teu
PC, e 12 palavras recuperam-na noutro lado. Não há máquina central a guardar as tuas
mensagens, nem sequer cifradas: quem tem o histórico é quem está online.

> **Estado: v0.1.x — utilizável para texto.** Já dá para entrar, criar um servidor, criar
> canais, convidar amigos e conversar. **Voz e partilha de ecrã ainda não passam entre peers**:
> a captura está validada, falta o transporte.

## Instalar

Descarrega o instalador da [última versão](https://github.com/Rakjsu/bruma/releases/latest).

O Windows vai avisar que o editor é desconhecido — o instalador não tem assinatura de código
comercial. *Mais informações* → *Executar mesmo assim*. A assinatura que existe garante que uma
**atualização** veio mesmo daqui, e é verificada pela app antes de instalar seja o que for.

A partir da v0.1.1 a app avisa sozinha quando há versão nova, e nunca instala sem perguntar.

## Porquê

O Discord funciona muito bem e não se pretende copiá-lo por desporto. O que muda aqui é o
modelo de confiança: ninguém precisa de dar dados para entrar, e não existe um sítio onde
o histórico de toda a gente fique acumulado à espera de ser pedido, vendido ou roubado.

## Como funciona, em cinco linhas

- **Rede**: [`iroh`](https://github.com/n0-computer/iroh) — QUIC peer-to-peer onde se marca
  por chave pública, não por IP. Hole-punch direto quando dá, relay quando não dá.
- **Identidade**: uma chave Ed25519 é ao mesmo tempo o teu ID e o teu endereço de rede.
- **Mensagens**: log append-only, assinado e encadeado por hash. O conteúdo é opaco.
- **Estado mutável** (canais, cargos, membros, reações): CRDT, desencriptado só localmente.
- **Voz e ecrã**: WebRTC mesh, com SFU opcional para grupos maiores.

O plano completo, com as decisões e — mais importante — as limitações que assume, está em
[`docs/PLANO.md`](docs/PLANO.md).

## Spikes

| Spike | Pergunta que responde | Estado |
|---|---|---|
| [1 · rede](spikes/spike1-net/) | Dois PCs em casas diferentes falam sem servidor? | código pronto, [à espera do teste real](docs/TESTE-COM-AMIGO.md) |
| [2 · ecrã](spikes/spike2-screen/) | Dá para partilhar ecrã com áudio dentro do Tauri, e a que custo? | **passa** — com picker, 4K@60; mas AV1 por software e sem ganho do contentHint |
| [3 · fantasma](spikes/spike3-ghost/) | Dá para sincronizar chat por `.onion` sem tor externo? | **bloqueado** — o arranque do Tor não conclui |

O Spike 1 vem primeiro de propósito: se a resposta for não, a arquitetura inteira muda e
tudo o resto seria trabalho deitado fora.

## Correr o Spike 1

Precisas de **duas máquinas em redes diferentes** — duas VMs em tua casa dão falso positivo.

```bash
cargo run -p spike1-net -- --name ana
```

Instruções completas e como ler o resultado: [`spikes/spike1-net/README.md`](spikes/spike1-net/README.md).
Para o teste que interessa mesmo — duas casas, duas redes — segue
[`docs/TESTE-COM-AMIGO.md`](docs/TESTE-COM-AMIGO.md); do outro lado basta um executável, sem
instalar nada.

## Correr o Spike 2

```bash
cargo run -p spike2-screen
```

Abre a aplicação. O diagnóstico da partilha de ecrã vive no canal `#diagnóstico`, e mede o bitrate
real do encoder por codec e por `contentHint`.
Detalhes em [`spikes/spike2-screen/README.md`](spikes/spike2-screen/README.md).

## Correr o Spike 3

```bash
cargo run -p spike3-ghost -- --name ana
```

Arranca um cliente Tor embutido e publica um onion service — sem daemon externo e sem abrir portas
no router. Detalhes em [`spikes/spike3-ghost/README.md`](spikes/spike3-ghost/README.md).

## O que o Bruma não faz

Isto está aqui à frente de propósito, porque prometer demais em privacidade é como estes
projetos morrem:

- **Numa chamada de voz em mesh, os participantes veem o teu IP.** O WebRTC faz o seu
  próprio NAT traversal. Esconder isso exige um relay TURN ou o SFU opcional.
- **O relay vê metadados** — que chaves falam entre si, quando e quanto. Nunca vê conteúdo.
- **Se ninguém do canal estiver online, não há sincronização.** É o preço de não haver servidor.
- **Um "ban" é criptográfico, não imposto.** Quem sai deixa de conseguir decifrar o que vier
  a seguir, mas continua a ter o que já tinha.
- **Não é anonimato de rede.** Para isso precisas de VPN ou Tor por baixo. O Modo Fantasma
  cobre o chat, mas desliga voz e ecrã — o Tor só transporta TCP.

## Licença

AGPL-3.0-or-later.

Bruma não tem qualquer ligação ao Discord Inc. Não usa o nome, o logo, os sons nem os
emojis deles.
