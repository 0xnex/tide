// 测试水龙头（devnet 原型用）：用模拟 USDC 的铸币权限钱包给一个测试钱包铸模拟 USDC，
// 并转一点 SOL 付手续费和租金。程序本身不变。网站上的领币按钮走 app/api/faucet.ts，规则相同。
//
// 用法（FUND_ADMIN 是基金管理员地址，默认就是运行脚本的钱包）：
//   ANCHOR_PROVIDER_URL=https://api.devnet.solana.com \
//   ANCHOR_WALLET=~/.config/solana/tide-devnet-faucet.json \
//   FUND_ADMIN=<基金管理员地址> \
//   yarn ts-node scripts/faucet.ts <测试钱包地址> [USDC 数量，默认 10000] [SOL 数量，默认 0.05]
import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { LAMPORTS_PER_SOL, PublicKey, SystemProgram, Transaction } from "@solana/web3.js";
import {
  createAssociatedTokenAccountIdempotentInstruction,
  createMintToInstruction,
  getAssociatedTokenAddressSync,
} from "@solana/spl-token";
import { Tide } from "../target/types/tide";

const IDL = require("../target/idl/tide.json");

async function main() {
  const [to, usdcArg, solArg] = process.argv.slice(2);
  if (!to) throw new Error("用法：faucet.ts <测试钱包地址> [USDC 数量] [SOL 数量]");
  const user = new PublicKey(to);
  const usdc = Number(usdcArg ?? 10_000);
  const sol = Number(solArg ?? 0.05);

  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = new Program<Tide>(IDL, provider);
  const admin = provider.wallet.publicKey; // 铸币权限钱包，付手续费和 SOL
  const fundAdmin = process.env.FUND_ADMIN ? new PublicKey(process.env.FUND_ADMIN) : admin;
  const [fund] = PublicKey.findProgramAddressSync([Buffer.from("fund"), fundAdmin.toBuffer()], program.programId);
  const { usdcMint } = await program.account.fund.fetch(fund);

  const ata = getAssociatedTokenAddressSync(usdcMint, user, true);
  const tx = new Transaction().add(
    createAssociatedTokenAccountIdempotentInstruction(admin, ata, user, usdcMint),
    createMintToInstruction(usdcMint, ata, admin, Math.round(usdc * 1_000_000)),
  );
  if (sol > 0) {
    tx.add(SystemProgram.transfer({ fromPubkey: admin, toPubkey: user, lamports: Math.round(sol * LAMPORTS_PER_SOL) }));
  }
  const sig = await provider.sendAndConfirm(tx);
  console.log(`给 ${user.toBase58()} 发了 ${usdc} 模拟 USDC 和 ${sol} SOL：${sig}`);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
