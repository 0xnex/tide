// Supabase Edge Function：每日结算（devnet）。由 supabase/crank-trigger.sql 里的 pg_cron 定时调用，
// 每一步做什么见 ../_shared/crank-core.ts。
//
// 这是源文件。部署前用 esbuild 按 Node 的方式把它和依赖打成一个文件 index.js（不进 git），见
// scripts/supabase-crank.sh：Pyth 的 solana-utils 的 ESM 版本有不带扩展名的导入，Deno 直接加载会失败，
// 打包后也只有一份 @solana/web3.js。
//
// 免费版函数一次最多运行 150 秒，所以每次最多运行约 110 秒：来不及做完的交易留给下一次调用，
// 定时任务在定价后的一个小时里调用好几次。重复调用是安全的：已经定过价就跳过，没有可交易的就停下。
//
// 函数的 Secrets（supabase secrets set，见 scripts/supabase-crank.sh）：
//   KEEPER_KEYPAIR  清算地址（也是网站用的基金的管理员）的私钥，solana-keygen 的 JSON 数组
//   PYTH_API_KEY    Hermes 的 API key（Pyth Terminal）
//   CRANK_SECRET    调用者要放在 x-crank-secret 头里的随机字符串（函数不校验 Supabase JWT）
//   RPC_URL         可选，默认 devnet
//   HERMES_URL      可选，默认 https://hermes.pyth.network
//
// 调用：POST，头 x-crank-secret。默认马上回答 202，结算在后台跑完，日志在 Supabase 的函数日志里；
// 加 ?wait=1 时等结算跑完，把日志放在回答里（手动检查用）。
declare const Deno: any;
import * as anchor from "@coral-xyz/anchor";
import { Connection, Keypair, Transaction, VersionedTransaction } from "@solana/web3.js";
import { runCrank } from "../_shared/crank-core";
import IDL from "../../../app/src/idl/tide.json";

const RUN_MS = 110_000;

function keeperWallet(kp: Keypair) {
  const sign = <T extends Transaction | VersionedTransaction>(tx: T): T => {
    if (tx instanceof VersionedTransaction) tx.sign([kp]);
    else tx.partialSign(kp);
    return tx;
  };
  return {
    publicKey: kp.publicKey,
    payer: kp,
    signTransaction: async <T extends Transaction | VersionedTransaction>(tx: T) => sign(tx),
    signAllTransactions: async <T extends Transaction | VersionedTransaction>(txs: T[]) => txs.map(sign),
  };
}

async function crank(log: (line: string) => void) {
  const env = (name: string) => Deno.env.get(name) ?? "";
  const kp = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(env("KEEPER_KEYPAIR"))));
  // 用运行时自带的 fetch；web3.js 默认的 node-fetch 在 Edge Runtime 里只是勉强能用
  const connection = new Connection(env("RPC_URL") || "https://api.devnet.solana.com", {
    commitment: "confirmed",
    fetch: (input, init) => fetch(input, init),
  });
  const provider = new anchor.AnchorProvider(connection, keeperWallet(kp) as any, { commitment: "confirmed" });
  await runCrank({
    provider,
    idl: IDL,
    pythApiKey: env("PYTH_API_KEY"),
    hermesUrl: env("HERMES_URL") || undefined,
    deadline: Date.now() + RUN_MS,
    log,
  });
}

Deno.serve(async (req) => {
  const secret = Deno.env.get("CRANK_SECRET");
  if (!secret || req.headers.get("x-crank-secret") !== secret) return new Response("Forbidden", { status: 403 });

  const lines: string[] = [];
  const log = (line: string) => {
    console.log(line);
    lines.push(line);
  };
  const job = crank(log).then(
    () => true,
    (e) => {
      console.error(e);
      lines.push(`失败：${e?.message ?? e}`);
      return false;
    },
  );
  if (new URL(req.url).searchParams.get("wait") !== "1") {
    // pg_net 默认 5 秒就不等了；让结算在回答之后继续跑完（本机的 edge-runtime 主服务没有 waitUntil，本来就会跑完）
    const runtime = (globalThis as any).EdgeRuntime;
    if (typeof runtime?.waitUntil === "function") runtime.waitUntil(job);
    return new Response("started\n", { status: 202 });
  }
  const ok = await job;
  return new Response(JSON.stringify({ ok, log: lines }, null, 2), {
    status: ok ? 200 : 500,
    headers: { "content-type": "application/json" },
  });
});
