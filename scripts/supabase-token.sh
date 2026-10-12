#!/usr/bin/env bash
# 把启动结算用的 GitHub token 写进 Supabase Vault（supabase/crank-trigger.sql 里的定时任务读它），不在终端显示。
#
# 先在仓库根目录的 .env（不进 git）里放两行：
#   SUPABASE_DB_URL=postgresql://postgres.<ref>:<密码>@aws-0-us-east-1.pooler.supabase.com:5432/postgres?sslmode=require
#   GITHUB_DISPATCH_TOKEN=github_pat_...   # 只授权 0xnex/tide，权限只有 Actions: Read and write
#
# 用法：scripts/supabase-token.sh          写入或更换 token
#       scripts/supabase-token.sh --test   写入后立刻按定时任务的方式启动一次结算，检查 GitHub 回答 204
set -euo pipefail
cd "$(dirname "$0")/.."
env_value() { grep "^$1=" .env | head -1 | cut -d= -f2-; }
DB_URL=$(env_value SUPABASE_DB_URL)
TOKEN=$(env_value GITHUB_DISPATCH_TOKEN)
[ -n "$DB_URL" ] || { echo ".env 里没有 SUPABASE_DB_URL" >&2; exit 1; }
[ -n "$TOKEN" ] || { echo ".env 里没有 GITHUB_DISPATCH_TOKEN" >&2; exit 1; }

psql "$DB_URL" -q -v ON_ERROR_STOP=1 -v tok="$TOKEN" <<'SQL'
delete from vault.secrets where name = 'github_dispatch_token';
select vault.create_secret(:'tok', 'github_dispatch_token') is not null as stored \gset
SQL
echo "token 已写入 Vault"

if [ "${1:-}" = "--test" ]; then
  # 和定时任务执行同一条语句，再等 pg_net 的回答
  CMD=$(psql "$DB_URL" -tA -c "select command from cron.job where jobname = 'tide-crank'")
  ID=$(psql "$DB_URL" -tA -c "$CMD" | tail -1)
  for _ in $(seq 30); do
    STATUS=$(psql "$DB_URL" -tA -c "select status_code from net._http_response where id = $ID")
    [ -n "$STATUS" ] && break
    sleep 1
  done
  echo "GitHub 回答：${STATUS:-没有回答}（204 表示结算已启动）"
  [ "$STATUS" = "204" ] || psql "$DB_URL" -tA -c "select content, error_msg from net._http_response where id = $ID"
fi
