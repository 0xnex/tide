// 测试水龙头（Vercel 函数，devnet 专用）：给一个地址铸 10,000 模拟 USDC，
// 地址里 SOL 不够付手续费时再送一点。每个地址只能领一次：以这个地址的 USDC 账户是否已经存在为准
// （创建账户的指令不是幂等的，两个请求同时到也只有一个成功）。
//
// 环境变量：
//   FAUCET_SECRET_KEY  水龙头钱包的私钥（solana-keygen 的 JSON 数组）；它是模拟 USDC 的铸币权限，
//                      自己付手续费和送出的 SOL
//   FUND_ADMIN         基金管理员地址（基金账户由它推出），默认是网站用的基金
//   RPC_URL            Solana RPC，默认 devnet
import {
  Connection,
  Keypair,
  LAMPORTS_PER_SOL,
  PublicKey,
  SystemProgram,
  Transaction,
  sendAndConfirmTransaction,
} from "@solana/web3.js";
import {
  createAssociatedTokenAccountInstruction,
  createMintToInstruction,
  getAssociatedTokenAddressSync,
} from "@solana/spl-token";

const PROGRAM_ID = new PublicKey("3E8fFGuHJnrKDyPH7ZXdkffq4CKryaHovizg4yUH1jNs");
const USDC_PER_ADDRESS = 10_000;
const SOL_TOP_UP = 0.02;
const SOL_ENOUGH = 0.01; // 已经有这么多 SOL 就不送

const reply = (status: number, body: object) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

export async function POST(request: Request): Promise<Response> {
  let user: PublicKey;
  try {
    user = new PublicKey(((await request.json()) as { address?: string }).address ?? "");
  } catch {
    return reply(400, { error: "Invalid address" });
  }
  const secret = process.env.FAUCET_SECRET_KEY;
  if (!secret) return reply(500, { error: "The faucet is not configured" });
  const faucet = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(secret)));
  const connection = new Connection(process.env.RPC_URL ?? "https://api.devnet.solana.com", "confirmed");

  // 基金账户里管理员之后就是 USDC mint：8 字节鉴别符 + admin 32 字节 + usdc_mint 32 字节
  const admin = new PublicKey(process.env.FUND_ADMIN ?? "9CYgaCMo2iWxRbuHmwXbSW475HhM1ikknTPA6nhpdAkq");
  const [fund] = PublicKey.findProgramAddressSync([Buffer.from("fund"), admin.toBuffer()], PROGRAM_ID);
  const fundInfo = await connection.getAccountInfo(fund);
  if (!fundInfo) return reply(500, { error: "Vault not found" });
  const usdcMint = new PublicKey(fundInfo.data.subarray(40, 72));

  const ata = getAssociatedTokenAddressSync(usdcMint, user, true);
  if (await connection.getAccountInfo(ata)) {
    return reply(409, { error: `Each address can claim once; this one already received ${USDC_PER_ADDRESS.toLocaleString("en-US")} test USDC` });
  }
  const tx = new Transaction().add(
    createAssociatedTokenAccountInstruction(faucet.publicKey, ata, user, usdcMint),
    createMintToInstruction(usdcMint, ata, faucet.publicKey, BigInt(USDC_PER_ADDRESS) * 1_000_000n),
  );
  const lamports = await connection.getBalance(user);
  const topUp = lamports < SOL_ENOUGH * LAMPORTS_PER_SOL;
  if (topUp) {
    tx.add(SystemProgram.transfer({ fromPubkey: faucet.publicKey, toPubkey: user, lamports: SOL_TOP_UP * LAMPORTS_PER_SOL }));
  }
  try {
    const signature = await sendAndConfirmTransaction(connection, tx, [faucet]);
    return reply(200, { signature, usdc: USDC_PER_ADDRESS, sol: topUp ? SOL_TOP_UP : 0 });
  } catch (e) {
    // 同时来的另一个请求已经建好了账户
    if (await connection.getAccountInfo(ata)) return reply(409, { error: "This address has already claimed" });
    return reply(502, { error: `Faucet transfer failed: ${(e as Error).message}` });
  }
}
