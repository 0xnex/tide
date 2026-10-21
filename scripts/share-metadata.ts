// 给基金的份额代币创建 Metaplex 元数据（名称、符号、图标），钱包里就能显示成基金份额。
// 管理员调用一次即可；之后要修改，管理员是元数据的更新权限，可以直接用 Metaplex 工具改。
//
// 用法：
//   ANCHOR_PROVIDER_URL=https://api.devnet.solana.com \
//   ANCHOR_WALLET=~/.config/solana/tide-devnet-admin-v3.json \
//   yarn ts-node scripts/share-metadata.ts           # 创建
//   yarn ts-node scripts/share-metadata.ts --update  # 已经创建过：改成新的名称、符号和地址（比如网站换了域名）
// 环境变量（可选）：SHARE_NAME、SHARE_SYMBOL、SHARE_URI（元数据 JSON 的地址）
import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { PublicKey, Transaction, TransactionInstruction } from "@solana/web3.js";
import { Tide } from "../target/types/tide";

const IDL = require("../target/idl/tide.json");
const TOKEN_METADATA = new PublicKey("metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s");

const NAME = process.env.SHARE_NAME ?? "Tide BTC Trend Share";
const SYMBOL = process.env.SHARE_SYMBOL ?? "tBTCT";
const URI = process.env.SHARE_URI ?? "https://btctide.vercel.app/share.json";

function metadataAddress(programId: PublicKey, admin: PublicKey): PublicKey {
  const pda = (seeds: Buffer[], owner = programId) => PublicKey.findProgramAddressSync(seeds, owner)[0];
  const fund = pda([Buffer.from("fund"), admin.toBuffer()]);
  const shareMint = pda([Buffer.from("share_mint"), fund.toBuffer()]);
  return pda([Buffer.from("metadata"), TOKEN_METADATA.toBuffer(), shareMint.toBuffer()], TOKEN_METADATA);
}

/** Metaplex UpdateMetadataAccountV2：管理员是更新权限，直接签名，不经过基金程序 */
export async function updateShareMetadata(provider: anchor.AnchorProvider, programId: PublicKey) {
  const str = (s: string) => {
    const b = Buffer.from(s, "utf8");
    const len = Buffer.alloc(4);
    len.writeUInt32LE(b.length);
    return Buffer.concat([len, b]);
  };
  const data = Buffer.concat([
    Buffer.from([15, 1]), // 指令编号 15，Some(DataV2)
    str(NAME),
    str(SYMBOL),
    str(URI),
    Buffer.from([0, 0, 0, 0, 0]), // 版税 0（u16），没有 creators、collection、uses
    Buffer.from([0, 0, 0]), // 更新权限、primary_sale_happened、is_mutable 都不变
  ]);
  const ix = new TransactionInstruction({
    programId: TOKEN_METADATA,
    keys: [
      { pubkey: metadataAddress(programId, provider.wallet.publicKey), isSigner: false, isWritable: true },
      { pubkey: provider.wallet.publicKey, isSigner: true, isWritable: false },
    ],
    data,
  });
  return provider.sendAndConfirm(new Transaction().add(ix));
}

export async function initShareMetadata(program: Program<Tide>, admin: PublicKey) {
  const pda = (seeds: Buffer[], programId = program.programId) => PublicKey.findProgramAddressSync(seeds, programId)[0];
  const fund = pda([Buffer.from("fund"), admin.toBuffer()]);
  const shareMint = pda([Buffer.from("share_mint"), fund.toBuffer()]);
  const metadata = pda([Buffer.from("metadata"), TOKEN_METADATA.toBuffer(), shareMint.toBuffer()], TOKEN_METADATA);
  return program.methods
    .initShareMetadata(NAME, SYMBOL, URI)
    .accountsPartial({ admin, fund, shareMint, metadata, tokenMetadataProgram: TOKEN_METADATA })
    .rpc();
}

if (require.main === module) {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = new Program<Tide>(IDL, provider);
  const update = process.argv.includes("--update");
  (update ? updateShareMetadata(provider, program.programId) : initShareMetadata(program, provider.wallet.publicKey))
    .then((sig) => console.log(`份额元数据已${update ? "更新" : "创建"}：${sig}`))
    .catch((e) => {
      console.error(e);
      process.exit(1);
    });
}
