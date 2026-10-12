-- 每天 01:05 UTC 准时启动 GitHub Actions 的每日结算（.github/workflows/crank.yml）。
-- GitHub 自己的定时任务常常晚几十分钟才跑；手动启动（workflow_dispatch）几秒内就开始，
-- 所以由 Supabase 的 pg_cron 按时调用 GitHub API 启动它。结算本身还在 GitHub Actions 里跑。
--
-- 需要 Vault 里的密钥 github_dispatch_token：只授权 0xnex/tide、权限只有 Actions: Read and write 的
-- fine-grained token。写入（或更换）见 scripts/supabase-token.sh。
--
-- 在 Supabase 项目里执行一次：psql "$SUPABASE_DB_URL" -f supabase/crank-trigger.sql（可以重复执行）
create extension if not exists pg_cron;
create extension if not exists pg_net;

select cron.schedule(
  'tide-crank',
  '5 1 * * *',
  $$
  select net.http_post(
    url := 'https://api.github.com/repos/0xnex/tide/actions/workflows/crank.yml/dispatches',
    headers := jsonb_build_object(
      'Authorization', 'Bearer ' || (select decrypted_secret from vault.decrypted_secrets where name = 'github_dispatch_token'),
      'Accept', 'application/vnd.github+json',
      'X-GitHub-Api-Version', '2022-11-28',
      'User-Agent', 'tide-crank-trigger',
      'Content-Type', 'application/json'
    ),
    body := '{"ref": "main"}'::jsonb
  );
  $$
);
