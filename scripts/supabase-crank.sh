#!/usr/bin/env bash
# 打包并部署 Supabase 结算函数（supabase/functions/crank），可选同时设置它的 Secrets。
#
# 用法：scripts/supabase-crank.sh             打包、部署
#       scripts/supabase-crank.sh --secrets   先设置 Secrets 再打包、部署
#       scripts/supabase-crank.sh --run       部署后调用一次并等结果（会真的结算，重复调用是安全的）
# Secrets 的来源：KEEPER_KEYPAIR 读 $KEEPER_KEYPAIR_FILE（默认 ~/.config/solana/tide-devnet-admin.json），
# PYTH_API_KEY、SUPABASE_CRANK_SECRET 和 SUPABASE_DB_URL（写 Vault 用）读仓库根目录的 .env（不进 git）。
# 值不会显示在终端里。定时任务本身见 supabase/crank-schedule.sql。
set -euo pipefail
cd "$(dirname "$0")/.."
REF=${SUPABASE_PROJECT_REF:-wkklkegrgdcyiwfgdbgf}
env_value() { grep "^$1=" .env | head -1 | cut -d= -f2-; }

if [[ " $* " == *" --secrets "* ]]; then
  tmp=$(mktemp)
  trap 'rm -f "$tmp"' EXIT
  chmod 600 "$tmp"
  {
    echo "KEEPER_KEYPAIR=$(tr -d ' \n' < "${KEEPER_KEYPAIR_FILE:-$HOME/.config/solana/tide-devnet-admin.json}")"
    echo "PYTH_API_KEY=$(env_value PYTH_API_KEY)"
    echo "CRANK_SECRET=$(env_value SUPABASE_CRANK_SECRET)"
  } > "$tmp"
  supabase secrets set --project-ref "$REF" --env-file "$tmp" > /dev/null
  # 定时任务从 Vault 读同一个调用密钥（supabase/crank-schedule.sql）
  psql "$(env_value SUPABASE_DB_URL)" -q -v ON_ERROR_STOP=1 -v sec="$(env_value SUPABASE_CRANK_SECRET)" <<'SQL'
delete from vault.secrets where name = 'crank_secret';
select vault.create_secret(:'sec', 'crank_secret') is not null as stored \gset
SQL
  echo "Secrets 已设置（函数和 Vault）"
fi

# 按 Node 的方式打包（Deno 直接加载 Pyth 的 ESM 依赖会失败）；Deno 没有的 Node 全局变量在开头补上
BANNER='import { createRequire as __cr } from "node:module"; import { fileURLToPath as __f } from "node:url"; import { Buffer as __B } from "node:buffer"; import __p from "node:process"; const require = __cr(import.meta.url); const __filename = __f(import.meta.url); const __dirname = __filename.replace(/[\\/][^\\/]*$/, ""); globalThis.global ??= globalThis; globalThis.Buffer ??= __B; globalThis.process ??= __p;'
npx -y esbuild@0.25.10 supabase/functions/crank/handler.ts --bundle --platform=node --format=esm --target=es2022 \
  --outfile=supabase/functions/crank/index.js --log-level=warning --banner:js="$BANNER"
# Supabase 的打包只接受带 node: 前缀的内置模块
sed -i.bak -E 's/from "(assert|buffer|crypto|events|fs|http|https|net|os|path|stream|tls|url|util|zlib)"/from "node:\1"/g' \
  supabase/functions/crank/index.js && rm supabase/functions/crank/index.js.bak
supabase functions deploy crank --project-ref "$REF" --use-api

if [[ " $* " == *" --run "* ]]; then
  curl -sS -X POST "https://$REF.supabase.co/functions/v1/crank" -H "x-crank-secret: $(env_value SUPABASE_CRANK_SECRET)"
  echo
fi
