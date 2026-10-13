// 每日结算脚本（devnet 原型用），在本机或 GitHub Actions 里手动运行。
// 每天的定时结算由 Supabase 函数 supabase/functions/crank 运行；两边共用 supabase/functions/_shared/crank-core.ts，
// 那里有每一步做什么的说明。这个脚本不限时：一直交易到程序回答没有可交易的为止。
//
// 用法：
//   ANCHOR_PROVIDER_URL=https://api.devnet.solana.com \
//   ANCHOR_WALLET=~/.config/solana/id.json \
//   PYTH_API_KEY=<key> \
//   yarn ts-node scripts/crank.ts
// Hermes 要 API key（从 Pyth Terminal 申请），HERMES_URL 可以换成别的 Hermes 服务地址。
import * as anchor from "@coral-xyz/anchor";
import { runCrank } from "../supabase/functions/_shared/crank-core";

const IDL = require("../target/idl/tide.json");

const provider = anchor.AnchorProvider.env();
anchor.setProvider(provider);
runCrank({ provider, idl: IDL, pythApiKey: process.env.PYTH_API_KEY ?? "", hermesUrl: process.env.HERMES_URL }).catch(
  (e) => {
    console.error(e);
    process.exit(1);
  },
);
