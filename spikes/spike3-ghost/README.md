# Spike 3 — Modo Fantasma

Prova se o **Modo Fantasma** do plano é exequível: chat a sincronizar por onion services, com o
cliente Tor embutido na própria aplicação.

| # | Afirmação | Como se vê |
|---|---|---|
| 1 | Tor **dentro** da app, sem daemon externo | não instalaste tor nenhum e funciona |
| 2 | **Sem abrir portas** no router | onion services atravessam NAT por desenho |
| 3 | Ninguém fica a saber o teu IP | não há relay a ver-te, nem o peer |
| 4 | Muda **só o transporte** | a cripto e o log vêm de `spike-common`, sem uma linha diferente |

O ponto 4 é o mais importante para o projeto. Se o Tor obrigasse a uma variante da criptografia, a
abstração de transporte do plano estaria errada e o Modo Fantasma seria uma segunda aplicação
disfarçada em vez de um botão.

## Estado: BLOQUEADO (21/08/2026)

O arranque do Tor **não conclui nesta máquina**. Não é lentidão: em dez minutos o processo
produziu nove linhas de log, todas dos dois primeiros segundos, e o `dir.sqlite3` ficou nos
65 536 bytes da base vazia — nem um byte gravado.

O que **funciona**, e não é pouco:

- o `arti` embebe-se no processo, sem daemon externo;
- liga-se a relays reais desta rede e faz handshake em menos de um segundo (chega aos 15%);
- o consenso do diretório **é recebido com sucesso** (`received 1 useful responses`);
- o pool de circuitos continua a trabalhar de 30 em 30 segundos, portanto o cliente está vivo.

O que **falha**: entre receber o consenso e o gravar, o gestor de diretório emudece por completo
— mesmo com o nível em `trace` — e o `create_bootstrapped()` nunca retorna. Sem erro, sem aviso,
sem `panic`.

### Hipóteses testadas e eliminadas

Registadas para ninguém as repetir:

| Hipótese | Como foi eliminada |
|---|---|
| O operador bloqueia o Tor | 7 das 9 autoridades de diretório respondem desta rede |
| Descarregamento lento | zero bytes gravados em dez minutos, não é lentidão |
| Relógio dessincronizado | desvio de 0,5 s contra hora da rede (o Tor rejeita consensos fora da janela) |
| `fs-mistrust` a barrar o acesso | igual com `ARTI_FS_DISABLE_PERMISSION_CHECKS=1` |
| Runtime tokio aninhado / não conduzido | igual passando `PreferredRuntime::current()` a `TorClient::with_runtime` |

A última era a mais plausível — o `arti` grava em SQLite por `spawn_blocking`, e trabalho enviado
para um runtime que ninguém conduz nunca volta, o que produziria exatamente este padrão. Não era.

### Próximo passo, se e quando se voltar a isto

Isolar num programa de ~20 linhas que só faça `create_bootstrapped()` e levá-lo ao projeto Tor.
A esta altura a suspeita é de um problema do `arti` no Windows, **não do Bruma** — e nesse caso é
um relatório de bug deles, não trabalho deste projeto.

Nada na Fase 1 depende disto. O Modo Fantasma estava planeado para a Fase 2.

## Correr

```bash
cargo run -p spike3-ghost -- --name ana
```

Depois de arrancar o Tor — pode levar 30 a 60 segundos — imprime um endereço `.onion`. Passa-o ao
outro lado:

```bash
cargo run -p spike3-ghost -- --name rui --connect <ENDERECO>.onion
```

Escrevem os dois e carregam Enter, tal como no Spike 1.

> **Tem paciência na primeira tentativa.** Depois de o serviço arrancar, o descritor ainda tem de
> ser publicado na rede Tor, o que leva mais 30 a 60 segundos. Se o outro lado falhar logo à
> primeira, espera um pouco e tenta de novo — não é bug.

## O que medir

Anota os tempos, porque são eles que determinam se isto é usável como funcionalidade e não só como
curiosidade:

1. **Arranque do Tor** — o programa imprime quanto demorou o bootstrap.
2. **Publicação do serviço** — quanto tempo até o outro lado conseguir ligar-se.
3. **Latência por mensagem** — escreve e vê quanto demora a aparecer do outro lado.

A expectativa do plano é 200–800 ms por mensagem. Se for muito pior, o Modo Fantasma continua a
servir para conversas assíncronas mas não para conversa a sério, e a UI tem de dizer isso.

## O que confirmar sobre privacidade

- **Nenhum dos lados abriu portas.** Não mexeste no router nem na firewall.
- **O endereço `.onion` é o único ponto de contacto.** Não há IP em lado nenhum do processo.
- O `data/<perfil>-log.json` continua opaco, exatamente como no Spike 1.

Repara numa coisa que o arti faz bem: **os endereços `.onion` são redactados nos logs por omissão**.
Para os mostrar é preciso pedir explicitamente `display_unredacted()`. É uma decisão de segurança
deles que vale a pena imitar no Bruma — o pior sítio para um identificador sensível aparecer é um
ficheiro de registo que alguém cola num fórum a pedir ajuda.

## A diferença de autenticação face ao Spike 1

Vale a pena perceber isto, porque muda o código do handshake:

- No **Spike 1**, o iroh autentica o peer por certificado TLS: `conn.remote_id()` devolve uma
  identidade já provada pelo transporte.
- No **Spike 3**, o Tor autentica o **endereço do serviço**, não a pessoa. Quem se liga sabe que
  chegou ao `.onion` certo, mas o serviço não sabe quem se ligou.

Por isso aqui a identidade viaja no `Hello` e é a **assinatura da prekey** que a prova. Sem essa
assinatura, qualquer um podia anunciar a chave pública de outra pessoa.

## O que este spike NÃO é

- **Não tem voz nem ecrã.** Não é limitação da app: o Tor só transporta TCP, e o WebRTC precisa de
  UDP. A proposta 348 do Tor, que traria UDP, nunca foi implantada. Isto é permanente, não temporário.
- **Só liga dois peers**, sem descoberta. Grupos são trabalho da Fase 1.
- **Guarda a semente em claro** em `data/<perfil>.key`, como os outros spikes.
- **Deixa o estado do Tor em `data/<perfil>-tor/`.** Apagar essa pasta gera um `.onion` novo.

Código descartável. O que sobrevive é a resposta e o desenho do handshake.
