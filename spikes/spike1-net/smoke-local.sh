#!/usr/bin/env bash
# Teste de fumo local, com dois processos na MESMA maquina.
#
# O que prova: handshake, cifra, log assinado, sync, e o resync depois de um
# peer estar offline.
# O que NAO prova: a questao do CGNAT. Mesma maquina = mesmo NAT. Para isso e
# preciso o teste com o amigo noutra casa (ver README.md).

set -u
cd "$(dirname "$0")"
ROOT="$(cd ../.. && pwd)"
BIN="$ROOT/target/debug/spike1-net.exe"
[ -x "$BIN" ] || BIN="$ROOT/target/debug/spike1-net"
[ -x "$BIN" ] || { echo "FALHA: binario nao encontrado. Corre: cargo build -p spike1-net"; exit 1; }

OUT="$(mktemp -d)"
rm -rf data && mkdir -p data
echo "logs em: $OUT"
echo

falhas=0
verifica() { # descricao, ficheiro, padrao
  if grep -qF "$2" "$3"; then
    echo "  [ok] $1"
  else
    echo "  [FALHA] $1"
    falhas=$((falhas + 1))
  fi
}

# --- ana: fica a espera o tempo todo ---
( sleep 20; echo "ana fala com o rui ligado"
  sleep 18; echo "ana escreve com o rui OFFLINE (1)"; echo "ana escreve com o rui OFFLINE (2)"
  sleep 25 ) | "$BIN" --name ana > "$OUT/ana.log" 2>&1 &
ANA_PID=$!

echo "[fase 0] a espera que a ana fique online..."
sleep 12
ID="$(grep -m1 '^identidade ' "$OUT/ana.log" | awk '{print $3}')"
if [ -z "$ID" ]; then
  echo "FALHA: nao consegui ler o EndpointId da ana"; cat "$OUT/ana.log"; kill $ANA_PID 2>/dev/null; exit 1
fi
echo "        EndpointId da ana: $ID"

# --- fase 1: rui liga-se, trocam mensagens ---
echo "[fase 1] rui liga-se e trocam mensagens (~16s)"
( sleep 6; echo "rui fala com a ana"; sleep 10 ) \
  | "$BIN" --name rui --connect "$ID" > "$OUT/rui1.log" 2>&1
echo "        rui saiu."

# --- fase 2: ana escreve sozinha (rui offline) ---
echo "[fase 2] ana escreve com o rui offline (~14s)"
sleep 14

# --- fase 3: rui volta e tem de apanhar o que perdeu ---
echo "[fase 3] rui volta e sincroniza (~15s)"
( sleep 15 ) | "$BIN" --name rui --connect "$ID" > "$OUT/rui2.log" 2>&1
kill $ANA_PID 2>/dev/null
wait $ANA_PID 2>/dev/null

echo
echo "=============== RESULTADO ==============="
echo "-- ligacao e cripto --"
verifica "handshake concluido"            "Chave de sessao estabelecida" "$OUT/rui1.log"
verifica "prekey do peer verificada"      "prekey assinada e verificada" "$OUT/rui1.log"

echo "-- troca ao vivo --"
verifica "rui recebeu a mensagem da ana"  "ana fala com o rui ligado"    "$OUT/rui1.log"
verifica "ana recebeu a mensagem do rui"  "rui fala com a ana"           "$OUT/ana.log"

echo "-- o ponto que interessa: resync depois de offline --"
verifica "rui apanhou a mensagem 1 que perdeu" "rui OFFLINE (1)" "$OUT/rui2.log"
verifica "rui apanhou a mensagem 2 que perdeu" "rui OFFLINE (2)" "$OUT/rui2.log"

echo "-- opacidade em disco --"
if grep -qF "ana fala com o rui" data/ana-log.json 2>/dev/null; then
  echo "  [FALHA] texto em claro encontrado no log em disco!"
  falhas=$((falhas + 1))
else
  echo "  [ok] nenhum texto em claro no log em disco"
fi

echo "-- caminho de rede (informativo, mesma maquina) --"
grep -hE 'caminho inicial|passou a DIRETO|voltou para RELAY' "$OUT"/*.log | sed 's/^/  /' | sort -u

echo "========================================="
if [ "$falhas" -eq 0 ]; then
  echo "TUDO OK -- o protocolo aguenta-se. Falta o teste em redes diferentes."
else
  echo "$falhas verificacao(oes) falharam. Logs: $OUT"
fi
exit "$falhas"
