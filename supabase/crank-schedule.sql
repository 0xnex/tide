-- 每日结算的定时任务：pg_cron 在 01:05–01:55 UTC 每 10 分钟调用一次结算函数 supabase/functions/crank。
-- 第一次调用定价并开始交易；函数每次最多运行约 110 秒，没做完的交易由后面几次接着做，
-- 没有事可做时这几次调用只确认一下就结束（已经定过价就跳过，没有可交易的就停）。
--
-- 需要 Vault 里的密钥 crank_secret（和函数的 Secret CRANK_SECRET 相同），由 scripts/supabase-crank.sh --secrets 写入。
--
-- 在 Supabase 项目里执行一次：psql "$SUPABASE_DB_URL" -f supabase/crank-schedule.sql（可以重复执行）
-- 查最近几次调用的结果（pg_net 只保留 6 小时）：
--   select created, status_code, content::json->'log' from net._http_response order by created desc limit 6;
create extension if not exists pg_cron;
create extension if not exists pg_net;

select cron.schedule(
  'tide-crank',
  '5,15,25,35,45,55 1 * * *',
  $$
  select net.http_post(
    url := 'https://wkklkegrgdcyiwfgdbgf.supabase.co/functions/v1/crank',
    headers := jsonb_build_object(
      'x-crank-secret', (select decrypted_secret from vault.decrypted_secrets where name = 'crank_secret'),
      'Content-Type', 'application/json'
    ),
    body := '{}'::jsonb,
    -- 函数等结算这次运行完才回答（最多约 110 秒）；pg_net 在后台等，不占数据库
    timeout_milliseconds := 150000
  );
  $$
);
