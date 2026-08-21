# Spike 1 — a premissa serverless

Este spike existe para responder a **uma** pergunta, antes de se escrever produto nenhum:

> Dois PCs em casas diferentes conseguem falar só pela chave pública, sem servidor,
> e continuar a funcionar depois de um deles desligar?

Se a resposta for não, o Bruma não pode ser serverless e todo o resto do plano muda.
É por isso que este é o Spike 1 e não o 2.

## O que ele prova

| # | Afirmação | Como se vê |
|---|---|---|
| 1 | Liga por **chave pública**, nunca por IP | só se passa o `EndpointId` ao outro lado |
| 2 | A ligação é **direta** ou **por relay** | linhas `[ok] DIRETO` / `[!] RELAY` no ecrã |
| 3 | O conteúdo é **opaco** | `cat data/<perfil>-log.json` — só hex |
| 4 | Sobrevive a estar **offline** | fecha um lado, escreve no outro, volta a abrir |

## Como correr

Precisas de **duas máquinas em redes diferentes**. Duas VMs em tua casa **não servem** —
partilham o mesmo NAT e dão um falso positivo.

Máquina A:

```bash
cargo run -p spike1-net -- --name ana
```

Ela imprime o `identidade : <ENDPOINT_ID>`. Passa esse ID para a máquina B (Signal, SMS, o que for):

```bash
cargo run -p spike1-net -- --name rui --connect <ENDPOINT_ID>
```

Escreve texto e carrega Enter dos dois lados. Deve aparecer no outro.

## O teste que interessa (offline → resync)

1. Com os dois ligados, troca duas ou três mensagens.
2. **Fecha a máquina B** (Ctrl+C).
3. Na máquina A, escreve mais três mensagens. Elas ficam no log local dela.
4. Volta a abrir a B com o mesmo `--name rui --connect <ID>`.
5. A B tem de mostrar as três mensagens que perdeu, na ordem certa.

Isto é o "guardar até a outra pessoa receber" a funcionar **sem servidor**: quem tem o
histórico é quem está online, não uma máquina no meio.

## Ler o veredito de CGNAT

O hole-punch acontece **depois** de a ligação abrir — normalmente nos primeiros segundos.
Por isso o programa vigia o caminho e avisa quando muda:

```
[!] caminho inicial: RELAY (hole-punch nao passou -- sinal de CGNAT)
[ok] passou a DIRETO (...) -- hole-punch feito
```

- **Passou a DIRETO** → hole-punch funciona nas vossas redes. A premissa aguenta-se.
- **Fica em RELAY para sempre** → pelo menos um dos lados está atrás de CGNAT.
  Continua a funcionar, mas todo o tráfego passa pelo relay público do n0, que não tem
  garantias de serviço para nós. Nesse caso a decisão é alojar um `iroh-relay` próprio —
  e essa decisão toma-se aqui, não daqui a três meses.

Vale a pena repetir o teste em várias combinações: fibra ↔ fibra, fibra ↔ dados móveis.
Os dados móveis são quase sempre CGNAT, portanto testam logo o pior caso.

## Provar que o conteúdo é opaco

Com o spike a correr, noutro terminal:

```bash
cat spikes/spike1-net/data/ana-log.json
```

Os campos `ciphertext` são hex sem estrutura. O ficheiro está de propósito em JSON com hex
para se poder olhar e confirmar que não há texto legível lá dentro.

## O que este spike NÃO é

- **Não tem forward secrecy.** A chave de sessão vem de um ECDH estático-estático entre as
  duas identidades. Deliberado: o objetivo é o transporte, não a cripto final. No produto
  isto é substituído por chaves de época atrás do trait `GroupKeyAgreement`.
- **Sincroniza o log inteiro** em cada ligação, em vez de só o delta. Chega para provar o
  ponto e é muito mais fácil de ler.
- **Só liga dois peers.** Grupos, gossip e canais são trabalho da Fase 1.
- **Guarda a semente em claro** em `data/<perfil>.key`. No produto vai para o keychain do SO.

É código descartável. Não o promovas a produto — reescreve-se em `crates/bruma-node`.
