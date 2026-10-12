// 给基金的份额代币创建 Metaplex 元数据（名称、符号、图标），钱包里就能显示成基金份额。
// 管理员调用一次即可；之后要修改，管理员是元数据的更新权限，可以直接用 Metaplex 工具改。
//
// 用法：
//   ANCHOR_PROVIDER_URL=https://api.devnet.solana.com \
//   ANCHOR_WALLET=~/.config/solana/tide-devnet-admin.json \
//   yarn ts-node scripts/share-metadata.ts
// 环境变量（可选）：SHARE_NAME、SHARE_SYMBOL、SHARE_URI（元数据 JSON 的地址）
import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { PublicKey } from "@solana/web3.js";
import { Tide } from "../target/types/tide";

const IDL = require("../target/idl/tide.json");
const TOKEN_METADATA = new PublicKey("metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s");

export async function initShareMetadata(program: Program<Tide>, admin: PublicKey) {
  const pda = (seeds: Buffer[], programId = program.programId) => PublicKey.findProgramAddressSync(seeds, programId)[0];
  const fund = pda([Buffer.from("fund"), admin.toBuffer()]);
  const shareMint = pda([Buffer.from("share_mint"), fund.toBuffer()]);
  const metadata = pda([Buffer.from("metadata"), TOKEN_METADATA.toBuffer(), shareMint.toBuffer()], TOKEN_METADATA);
  return program.methods
    .initShareMetadata(
      process.env.SHARE_NAME ?? "Tide BTC Trend Share",
      process.env.SHARE_SYMBOL ?? "tBTCT",
      process.env.SHARE_URI ?? "https://app-ten-sable-99.vercel.app/share.json",
    )
    .accountsPartial({ admin, fund, shareMint, metadata, tokenMetadataProgram: TOKEN_METADATA })
    .rpc();
}

if (require.main === module) {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = new Program<Tide>(IDL, provider);
  initShareMetadata(program, provider.wallet.publicKey)
    .then((sig) => console.log(`份额元数据已创建：${sig}`))
    .catch((e) => {
      console.error(e);
      process.exit(1);
    });
}
