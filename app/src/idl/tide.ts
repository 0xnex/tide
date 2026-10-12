/**
 * Program IDL in camelCase format in order to be used in JS/TS.
 *
 * Note that this is only a type helper and is not the actual IDL. The original
 * IDL can be found at `target/idl/tide.json`.
 */
export type Tide = {
  "address": "3E8fFGuHJnrKDyPH7ZXdkffq4CKryaHovizg4yUH1jNs",
  "metadata": {
    "name": "tide",
    "version": "0.1.0",
    "spec": "0.1.0",
    "description": "BTC 50-day moving-average fund (devnet prototype)"
  },
  "instructions": [
    {
      "name": "applyConfig",
      "docs": [
        "时间锁过了之后让提议的配置生效"
      ],
      "discriminator": [
        121,
        42,
        53,
        8,
        51,
        150,
        105,
        249
      ],
      "accounts": [
        {
          "name": "admin",
          "signer": true,
          "relations": [
            "fund"
          ]
        },
        {
          "name": "fund",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  110,
                  100
                ]
              },
              {
                "kind": "account",
                "path": "admin"
              }
            ]
          }
        }
      ],
      "args": []
    },
    {
      "name": "backfillPrice",
      "docs": [
        "回填没有定价的那一天（day，日期序号）的信号价格，任何人都可以调用。",
        "传入那一天定价窗口内发布的 Pyth 历史价格更新，所以不检查是否过旧。"
      ],
      "discriminator": [
        176,
        240,
        196,
        104,
        68,
        118,
        125,
        43
      ],
      "accounts": [
        {
          "name": "caller",
          "signer": true
        },
        {
          "name": "fund",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  110,
                  100
                ]
              },
              {
                "kind": "account",
                "path": "fund.admin",
                "account": "fund"
              }
            ]
          }
        },
        {
          "name": "btcPriceUpdate",
          "optional": true
        },
        {
          "name": "cbbtcPriceUpdate",
          "optional": true
        }
      ],
      "args": [
        {
          "name": "day",
          "type": "i64"
        }
      ]
    },
    {
      "name": "cancel",
      "docs": [
        "关闭这一期的定价时刻之前撤销排队记录：申购款和赎回份额全额退回，记录关闭、退回租金"
      ],
      "discriminator": [
        232,
        219,
        223,
        41,
        219,
        236,
        220,
        190
      ],
      "accounts": [
        {
          "name": "user",
          "writable": true,
          "signer": true
        },
        {
          "name": "fund",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  110,
                  100
                ]
              },
              {
                "kind": "account",
                "path": "fund.admin",
                "account": "fund"
              }
            ]
          }
        },
        {
          "name": "ticket",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  116,
                  105,
                  99,
                  107,
                  101,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              },
              {
                "kind": "account",
                "path": "user"
              },
              {
                "kind": "account",
                "path": "ticket.epoch",
                "account": "ticket"
              }
            ]
          }
        },
        {
          "name": "shareMint"
        },
        {
          "name": "userShares",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "account",
                "path": "user"
              },
              {
                "kind": "const",
                "value": [
                  6,
                  221,
                  246,
                  225,
                  215,
                  101,
                  161,
                  147,
                  217,
                  203,
                  225,
                  70,
                  206,
                  235,
                  121,
                  172,
                  28,
                  180,
                  133,
                  237,
                  95,
                  91,
                  55,
                  145,
                  58,
                  140,
                  245,
                  133,
                  126,
                  255,
                  0,
                  169
                ]
              },
              {
                "kind": "account",
                "path": "shareMint"
              }
            ],
            "program": {
              "kind": "const",
              "value": [
                140,
                151,
                37,
                143,
                78,
                36,
                137,
                241,
                187,
                61,
                16,
                41,
                20,
                142,
                13,
                131,
                11,
                90,
                19,
                153,
                218,
                255,
                16,
                132,
                4,
                142,
                123,
                216,
                219,
                233,
                248,
                89
              ]
            }
          }
        },
        {
          "name": "userUsdc",
          "writable": true
        },
        {
          "name": "subscriptionEscrow",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  115,
                  117,
                  98,
                  115,
                  99,
                  114,
                  105,
                  112,
                  116,
                  105,
                  111,
                  110,
                  95,
                  101,
                  115,
                  99,
                  114,
                  111,
                  119
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              }
            ]
          }
        },
        {
          "name": "shareEscrow",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  115,
                  104,
                  97,
                  114,
                  101,
                  95,
                  101,
                  115,
                  99,
                  114,
                  111,
                  119
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              }
            ]
          }
        },
        {
          "name": "tokenProgram",
          "address": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        },
        {
          "name": "associatedTokenProgram",
          "address": "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL"
        },
        {
          "name": "systemProgram",
          "address": "11111111111111111111111111111111"
        }
      ],
      "args": []
    },
    {
      "name": "cancelPending",
      "docs": [
        "撤销已定价、还没成交完的申购，任何时候都可以：没成交的 USDC 从申购托管退回，",
        "已成交、还没领取的份额从份额托管转给用户。epoch_record 同 claim"
      ],
      "discriminator": [
        74,
        87,
        109,
        242,
        64,
        192,
        151,
        71
      ],
      "accounts": [
        {
          "name": "user",
          "writable": true,
          "signer": true
        },
        {
          "name": "fund",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  110,
                  100
                ]
              },
              {
                "kind": "account",
                "path": "fund.admin",
                "account": "fund"
              }
            ]
          },
          "relations": [
            "epochRecord"
          ]
        },
        {
          "name": "ticket",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  116,
                  105,
                  99,
                  107,
                  101,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              },
              {
                "kind": "account",
                "path": "user"
              },
              {
                "kind": "account",
                "path": "ticket.epoch",
                "account": "ticket"
              }
            ]
          }
        },
        {
          "name": "epochRecord",
          "docs": [
            "关闭这张排队记录那一期的定价记录；期号范围由 fund-logic 校验"
          ],
          "writable": true
        },
        {
          "name": "shareMint"
        },
        {
          "name": "userShares",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "account",
                "path": "user"
              },
              {
                "kind": "const",
                "value": [
                  6,
                  221,
                  246,
                  225,
                  215,
                  101,
                  161,
                  147,
                  217,
                  203,
                  225,
                  70,
                  206,
                  235,
                  121,
                  172,
                  28,
                  180,
                  133,
                  237,
                  95,
                  91,
                  55,
                  145,
                  58,
                  140,
                  245,
                  133,
                  126,
                  255,
                  0,
                  169
                ]
              },
              {
                "kind": "account",
                "path": "shareMint"
              }
            ],
            "program": {
              "kind": "const",
              "value": [
                140,
                151,
                37,
                143,
                78,
                36,
                137,
                241,
                187,
                61,
                16,
                41,
                20,
                142,
                13,
                131,
                11,
                90,
                19,
                153,
                218,
                255,
                16,
                132,
                4,
                142,
                123,
                216,
                219,
                233,
                248,
                89
              ]
            }
          }
        },
        {
          "name": "userUsdc",
          "writable": true
        },
        {
          "name": "subscriptionEscrow",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  115,
                  117,
                  98,
                  115,
                  99,
                  114,
                  105,
                  112,
                  116,
                  105,
                  111,
                  110,
                  95,
                  101,
                  115,
                  99,
                  114,
                  111,
                  119
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              }
            ]
          }
        },
        {
          "name": "shareEscrow",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  115,
                  104,
                  97,
                  114,
                  101,
                  95,
                  101,
                  115,
                  99,
                  114,
                  111,
                  119
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              }
            ]
          }
        },
        {
          "name": "tokenProgram",
          "address": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        },
        {
          "name": "associatedTokenProgram",
          "address": "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL"
        },
        {
          "name": "systemProgram",
          "address": "11111111111111111111111111111111"
        }
      ],
      "args": []
    },
    {
      "name": "claim",
      "docs": [
        "epoch_record 传关闭这张排队记录那一期的定价记录：通常期号相同；",
        "中间的定价被跳过时，是之后第一次定价的记录（first_epoch ≤ 排队记录的期号 ≤ epoch）。"
      ],
      "discriminator": [
        62,
        198,
        214,
        193,
        213,
        159,
        108,
        210
      ],
      "accounts": [
        {
          "name": "user",
          "writable": true,
          "signer": true
        },
        {
          "name": "fund",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  110,
                  100
                ]
              },
              {
                "kind": "account",
                "path": "fund.admin",
                "account": "fund"
              }
            ]
          },
          "relations": [
            "epochRecord"
          ]
        },
        {
          "name": "ticket",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  116,
                  105,
                  99,
                  107,
                  101,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              },
              {
                "kind": "account",
                "path": "user"
              },
              {
                "kind": "account",
                "path": "ticket.epoch",
                "account": "ticket"
              }
            ]
          }
        },
        {
          "name": "epochRecord",
          "docs": [
            "关闭这张排队记录那一期的定价记录；期号范围由 fund-logic 校验，这里校验它属于这个基金"
          ],
          "writable": true
        },
        {
          "name": "shareMint"
        },
        {
          "name": "usdcMint"
        },
        {
          "name": "userShares",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "account",
                "path": "user"
              },
              {
                "kind": "const",
                "value": [
                  6,
                  221,
                  246,
                  225,
                  215,
                  101,
                  161,
                  147,
                  217,
                  203,
                  225,
                  70,
                  206,
                  235,
                  121,
                  172,
                  28,
                  180,
                  133,
                  237,
                  95,
                  91,
                  55,
                  145,
                  58,
                  140,
                  245,
                  133,
                  126,
                  255,
                  0,
                  169
                ]
              },
              {
                "kind": "account",
                "path": "shareMint"
              }
            ],
            "program": {
              "kind": "const",
              "value": [
                140,
                151,
                37,
                143,
                78,
                36,
                137,
                241,
                187,
                61,
                16,
                41,
                20,
                142,
                13,
                131,
                11,
                90,
                19,
                153,
                218,
                255,
                16,
                132,
                4,
                142,
                123,
                216,
                219,
                233,
                248,
                89
              ]
            }
          }
        },
        {
          "name": "userUsdc",
          "writable": true
        },
        {
          "name": "shareEscrow",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  115,
                  104,
                  97,
                  114,
                  101,
                  95,
                  101,
                  115,
                  99,
                  114,
                  111,
                  119
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              }
            ]
          }
        },
        {
          "name": "payoutVault",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  112,
                  97,
                  121,
                  111,
                  117,
                  116,
                  95,
                  118,
                  97,
                  117,
                  108,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              }
            ]
          }
        },
        {
          "name": "subscriptionEscrow",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  115,
                  117,
                  98,
                  115,
                  99,
                  114,
                  105,
                  112,
                  116,
                  105,
                  111,
                  110,
                  95,
                  101,
                  115,
                  99,
                  114,
                  111,
                  119
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              }
            ]
          }
        },
        {
          "name": "tokenProgram",
          "address": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        },
        {
          "name": "associatedTokenProgram",
          "address": "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL"
        },
        {
          "name": "systemProgram",
          "address": "11111111111111111111111111111111"
        }
      ],
      "args": []
    },
    {
      "name": "collectFees",
      "docs": [
        "把欠管理人的取款费从待领取账户付到收费地址的 USDC 关联代币账户（不存在时由调用人创建）。",
        "任何人都可以调用：钱只能去配置里固定的收费地址"
      ],
      "discriminator": [
        164,
        152,
        207,
        99,
        30,
        186,
        19,
        182
      ],
      "accounts": [
        {
          "name": "caller",
          "writable": true,
          "signer": true
        },
        {
          "name": "fund",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  110,
                  100
                ]
              },
              {
                "kind": "account",
                "path": "fund.admin",
                "account": "fund"
              }
            ]
          }
        },
        {
          "name": "usdcMint"
        },
        {
          "name": "feeAddress"
        },
        {
          "name": "feeUsdc",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "account",
                "path": "feeAddress"
              },
              {
                "kind": "const",
                "value": [
                  6,
                  221,
                  246,
                  225,
                  215,
                  101,
                  161,
                  147,
                  217,
                  203,
                  225,
                  70,
                  206,
                  235,
                  121,
                  172,
                  28,
                  180,
                  133,
                  237,
                  95,
                  91,
                  55,
                  145,
                  58,
                  140,
                  245,
                  133,
                  126,
                  255,
                  0,
                  169
                ]
              },
              {
                "kind": "account",
                "path": "usdcMint"
              }
            ],
            "program": {
              "kind": "const",
              "value": [
                140,
                151,
                37,
                143,
                78,
                36,
                137,
                241,
                187,
                61,
                16,
                41,
                20,
                142,
                13,
                131,
                11,
                90,
                19,
                153,
                218,
                255,
                16,
                132,
                4,
                142,
                123,
                216,
                219,
                233,
                248,
                89
              ]
            }
          }
        },
        {
          "name": "payoutVault",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  112,
                  97,
                  121,
                  111,
                  117,
                  116,
                  95,
                  118,
                  97,
                  117,
                  108,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              }
            ]
          }
        },
        {
          "name": "tokenProgram",
          "address": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        },
        {
          "name": "associatedTokenProgram",
          "address": "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL"
        },
        {
          "name": "systemProgram",
          "address": "11111111111111111111111111111111"
        }
      ],
      "args": []
    },
    {
      "name": "fix",
      "docs": [
        "定价步骤：每天定价时刻之后调用一次。epoch 是当天的日期序号（unix 秒 / 86400），",
        "用来推出这次定价记录的地址"
      ],
      "discriminator": [
        158,
        52,
        140,
        227,
        138,
        59,
        214,
        94
      ],
      "accounts": [
        {
          "name": "cranker",
          "docs": [
            "清算窗口内只能是清算地址，之后任何人都可以；支付本期记录账户的租金"
          ],
          "writable": true,
          "signer": true
        },
        {
          "name": "fund",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  110,
                  100
                ]
              },
              {
                "kind": "account",
                "path": "fund.admin",
                "account": "fund"
              }
            ]
          }
        },
        {
          "name": "epochRecord",
          "docs": [
            "当天的定价记录。已经存在只可能是今天定价过了：这时由 fund-logic 回答 AlreadySettled，",
            "不会改写它（所以用 init_if_needed，而不是让创建账户失败）"
          ],
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  101,
                  112,
                  111,
                  99,
                  104
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              },
              {
                "kind": "arg",
                "path": "epoch"
              }
            ]
          }
        },
        {
          "name": "btcPriceUpdate",
          "optional": true
        },
        {
          "name": "cbbtcPriceUpdate",
          "optional": true
        },
        {
          "name": "shareMint",
          "writable": true,
          "relations": [
            "fund"
          ]
        },
        {
          "name": "usdcVault",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  117,
                  115,
                  100,
                  99,
                  95,
                  118,
                  97,
                  117,
                  108,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              }
            ]
          }
        },
        {
          "name": "subscriptionEscrow",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  115,
                  117,
                  98,
                  115,
                  99,
                  114,
                  105,
                  112,
                  116,
                  105,
                  111,
                  110,
                  95,
                  101,
                  115,
                  99,
                  114,
                  111,
                  119
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              }
            ]
          }
        },
        {
          "name": "shareEscrow",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  115,
                  104,
                  97,
                  114,
                  101,
                  95,
                  101,
                  115,
                  99,
                  114,
                  111,
                  119
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              }
            ]
          }
        },
        {
          "name": "payoutVault",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  112,
                  97,
                  121,
                  111,
                  117,
                  116,
                  95,
                  118,
                  97,
                  117,
                  108,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              }
            ]
          }
        },
        {
          "name": "tokenProgram",
          "address": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        },
        {
          "name": "systemProgram",
          "address": "11111111111111111111111111111111"
        }
      ],
      "args": [
        {
          "name": "epoch",
          "type": "u64"
        }
      ]
    },
    {
      "name": "initMockMarket",
      "discriminator": [
        172,
        96,
        12,
        22,
        196,
        111,
        83,
        200
      ],
      "accounts": [
        {
          "name": "authority",
          "writable": true,
          "signer": true
        },
        {
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "authority"
              }
            ]
          }
        },
        {
          "name": "usdcMint"
        },
        {
          "name": "btcMint"
        },
        {
          "name": "marketUsdc",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  117,
                  115,
                  100,
                  99
                ]
              },
              {
                "kind": "account",
                "path": "market"
              }
            ]
          }
        },
        {
          "name": "marketBtc",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  98,
                  116,
                  99
                ]
              },
              {
                "kind": "account",
                "path": "market"
              }
            ]
          }
        },
        {
          "name": "tokenProgram",
          "address": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        },
        {
          "name": "systemProgram",
          "address": "11111111111111111111111111111111"
        }
      ],
      "args": [
        {
          "name": "feeBps",
          "type": "u16"
        }
      ]
    },
    {
      "name": "initShareMetadata",
      "docs": [
        "给份额代币创建 Metaplex 元数据（名称、符号、元数据链接），钱包里就能显示名字和图标。",
        "份额的铸币权限是基金 PDA，所以由程序签名；更新权限给管理员，以后可以直接用 Metaplex 工具修改。"
      ],
      "discriminator": [
        58,
        123,
        101,
        232,
        8,
        161,
        154,
        116
      ],
      "accounts": [
        {
          "name": "admin",
          "writable": true,
          "signer": true,
          "relations": [
            "fund"
          ]
        },
        {
          "name": "fund",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  110,
                  100
                ]
              },
              {
                "kind": "account",
                "path": "admin"
              }
            ]
          }
        },
        {
          "name": "shareMint",
          "relations": [
            "fund"
          ]
        },
        {
          "name": "metadata",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  101,
                  116,
                  97,
                  100,
                  97,
                  116,
                  97
                ]
              },
              {
                "kind": "account",
                "path": "tokenMetadataProgram"
              },
              {
                "kind": "account",
                "path": "shareMint"
              }
            ],
            "program": {
              "kind": "account",
              "path": "tokenMetadataProgram"
            }
          }
        },
        {
          "name": "tokenMetadataProgram",
          "address": "metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s"
        },
        {
          "name": "systemProgram",
          "address": "11111111111111111111111111111111"
        },
        {
          "name": "rent",
          "address": "SysvarRent111111111111111111111111111111111"
        }
      ],
      "args": [
        {
          "name": "name",
          "type": "string"
        },
        {
          "name": "symbol",
          "type": "string"
        },
        {
          "name": "uri",
          "type": "string"
        }
      ]
    },
    {
      "name": "initVaults",
      "docs": [
        "第 2 步：创建基金的 5 个代币账户（分开做，避免单条指令账户过多）"
      ],
      "discriminator": [
        250,
        62,
        242,
        50,
        163,
        133,
        108,
        93
      ],
      "accounts": [
        {
          "name": "admin",
          "writable": true,
          "signer": true,
          "relations": [
            "fund"
          ]
        },
        {
          "name": "fund",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  110,
                  100
                ]
              },
              {
                "kind": "account",
                "path": "admin"
              }
            ]
          }
        },
        {
          "name": "usdcMint",
          "relations": [
            "fund"
          ]
        },
        {
          "name": "btcMint",
          "relations": [
            "fund"
          ]
        },
        {
          "name": "shareMint",
          "relations": [
            "fund"
          ]
        },
        {
          "name": "usdcVault",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  117,
                  115,
                  100,
                  99,
                  95,
                  118,
                  97,
                  117,
                  108,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              }
            ]
          }
        },
        {
          "name": "btcVault",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  98,
                  116,
                  99,
                  95,
                  118,
                  97,
                  117,
                  108,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              }
            ]
          }
        },
        {
          "name": "subscriptionEscrow",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  115,
                  117,
                  98,
                  115,
                  99,
                  114,
                  105,
                  112,
                  116,
                  105,
                  111,
                  110,
                  95,
                  101,
                  115,
                  99,
                  114,
                  111,
                  119
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              }
            ]
          }
        },
        {
          "name": "payoutVault",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  112,
                  97,
                  121,
                  111,
                  117,
                  116,
                  95,
                  118,
                  97,
                  117,
                  108,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              }
            ]
          }
        },
        {
          "name": "shareEscrow",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  115,
                  104,
                  97,
                  114,
                  101,
                  95,
                  101,
                  115,
                  99,
                  114,
                  111,
                  119
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              }
            ]
          }
        },
        {
          "name": "tokenProgram",
          "address": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        },
        {
          "name": "systemProgram",
          "address": "11111111111111111111111111111111"
        }
      ],
      "args": []
    },
    {
      "name": "initialize",
      "docs": [
        "第 1 步：创建基金账户和份额代币"
      ],
      "discriminator": [
        175,
        175,
        109,
        31,
        13,
        152,
        155,
        237
      ],
      "accounts": [
        {
          "name": "admin",
          "writable": true,
          "signer": true
        },
        {
          "name": "fund",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  110,
                  100
                ]
              },
              {
                "kind": "account",
                "path": "admin"
              }
            ]
          }
        },
        {
          "name": "usdcMint"
        },
        {
          "name": "btcMint"
        },
        {
          "name": "shareMint",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  115,
                  104,
                  97,
                  114,
                  101,
                  95,
                  109,
                  105,
                  110,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              }
            ]
          }
        },
        {
          "name": "market"
        },
        {
          "name": "tokenProgram",
          "address": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        },
        {
          "name": "systemProgram",
          "address": "11111111111111111111111111111111"
        }
      ],
      "args": [
        {
          "name": "config",
          "type": {
            "defined": {
              "name": "configData"
            }
          }
        }
      ]
    },
    {
      "name": "pauseSubscriptions",
      "docs": [
        "立即暂停申购。已经排队和待成交的申购照常处理，赎回、结算和领取不受影响"
      ],
      "discriminator": [
        63,
        142,
        254,
        5,
        106,
        80,
        227,
        85
      ],
      "accounts": [
        {
          "name": "admin",
          "signer": true,
          "relations": [
            "fund"
          ]
        },
        {
          "name": "fund",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  110,
                  100
                ]
              },
              {
                "kind": "account",
                "path": "admin"
              }
            ]
          }
        }
      ],
      "args": []
    },
    {
      "name": "proposeConfig",
      "docs": [
        "提议一份完整的新配置，至少 48 小时之后才能 apply_config。",
        "新的提议取代还没生效的旧提议、重新计时；要撤回就提议当前配置。"
      ],
      "discriminator": [
        163,
        247,
        238,
        160,
        236,
        129,
        153,
        160
      ],
      "accounts": [
        {
          "name": "admin",
          "signer": true,
          "relations": [
            "fund"
          ]
        },
        {
          "name": "fund",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  110,
                  100
                ]
              },
              {
                "kind": "account",
                "path": "admin"
              }
            ]
          }
        }
      ],
      "args": [
        {
          "name": "config",
          "type": {
            "defined": {
              "name": "configData"
            }
          }
        }
      ]
    },
    {
      "name": "requestRedemption",
      "docs": [
        "epoch 同 request_subscription"
      ],
      "discriminator": [
        14,
        62,
        182,
        237,
        59,
        79,
        149,
        22
      ],
      "accounts": [
        {
          "name": "user",
          "writable": true,
          "signer": true
        },
        {
          "name": "fund",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  110,
                  100
                ]
              },
              {
                "kind": "account",
                "path": "fund.admin",
                "account": "fund"
              }
            ]
          }
        },
        {
          "name": "ticket",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  116,
                  105,
                  99,
                  107,
                  101,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              },
              {
                "kind": "account",
                "path": "user"
              },
              {
                "kind": "arg",
                "path": "epoch"
              }
            ]
          }
        },
        {
          "name": "userShares",
          "writable": true
        },
        {
          "name": "shareEscrow",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  115,
                  104,
                  97,
                  114,
                  101,
                  95,
                  101,
                  115,
                  99,
                  114,
                  111,
                  119
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              }
            ]
          }
        },
        {
          "name": "tokenProgram",
          "address": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        },
        {
          "name": "systemProgram",
          "address": "11111111111111111111111111111111"
        }
      ],
      "args": [
        {
          "name": "shares",
          "type": "u64"
        },
        {
          "name": "epoch",
          "type": "u64"
        }
      ]
    },
    {
      "name": "requestSubscription",
      "docs": [
        "epoch 是这一刻开放的那一期（fund_logic::request_epoch），只用来推出排队记录的地址"
      ],
      "discriminator": [
        137,
        154,
        227,
        71,
        69,
        159,
        134,
        178
      ],
      "accounts": [
        {
          "name": "user",
          "writable": true,
          "signer": true
        },
        {
          "name": "fund",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  110,
                  100
                ]
              },
              {
                "kind": "account",
                "path": "fund.admin",
                "account": "fund"
              }
            ]
          }
        },
        {
          "name": "ticket",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  116,
                  105,
                  99,
                  107,
                  101,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              },
              {
                "kind": "account",
                "path": "user"
              },
              {
                "kind": "arg",
                "path": "epoch"
              }
            ]
          }
        },
        {
          "name": "userUsdc",
          "writable": true
        },
        {
          "name": "subscriptionEscrow",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  115,
                  117,
                  98,
                  115,
                  99,
                  114,
                  105,
                  112,
                  116,
                  105,
                  111,
                  110,
                  95,
                  101,
                  115,
                  99,
                  114,
                  111,
                  119
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              }
            ]
          }
        },
        {
          "name": "tokenProgram",
          "address": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        },
        {
          "name": "systemProgram",
          "address": "11111111111111111111111111111111"
        }
      ],
      "args": [
        {
          "name": "amount",
          "type": "u64"
        },
        {
          "name": "epoch",
          "type": "u64"
        }
      ]
    },
    {
      "name": "resumeSubscriptions",
      "discriminator": [
        120,
        151,
        211,
        124,
        149,
        243,
        220,
        218
      ],
      "accounts": [
        {
          "name": "admin",
          "signer": true,
          "relations": [
            "fund"
          ]
        },
        {
          "name": "fund",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  110,
                  100
                ]
              },
              {
                "kind": "account",
                "path": "admin"
              }
            ]
          }
        }
      ],
      "args": []
    },
    {
      "name": "seedPrices",
      "docs": [
        "首次定价前，管理员写入最近几天的历史价格（到昨天为止，最早的在前），",
        "让均线从第一天就能计算"
      ],
      "discriminator": [
        47,
        127,
        125,
        208,
        228,
        132,
        168,
        244
      ],
      "accounts": [
        {
          "name": "admin",
          "signer": true,
          "relations": [
            "fund"
          ]
        },
        {
          "name": "fund",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  110,
                  100
                ]
              },
              {
                "kind": "account",
                "path": "admin"
              }
            ]
          }
        }
      ],
      "args": [
        {
          "name": "prices",
          "type": {
            "vec": "u64"
          }
        }
      ]
    },
    {
      "name": "setMockMarketFee",
      "docs": [
        "修改模拟交易所的手续费，用来模拟成交低于 min_out"
      ],
      "discriminator": [
        5,
        122,
        36,
        52,
        97,
        225,
        247,
        78
      ],
      "accounts": [
        {
          "name": "authority",
          "signer": true,
          "relations": [
            "market"
          ]
        },
        {
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "authority"
              }
            ]
          }
        }
      ],
      "args": [
        {
          "name": "feeBps",
          "type": "u16"
        }
      ]
    },
    {
      "name": "trade",
      "docs": [
        "一笔交易（或一次不交易的申购成交）。epoch_record 传最早一期未卖完的赎回资产的记录",
        "（fund.state.slices[0].epoch）；没有赎回资产时传最早一期待成交申购的记录",
        "（fund.state.pending[0].epoch）；都没有时不传。基金调仓用不到它。"
      ],
      "discriminator": [
        178,
        144,
        26,
        216,
        241,
        187,
        206,
        130
      ],
      "accounts": [
        {
          "name": "cranker",
          "docs": [
            "清算窗口内只能是清算地址，之后任何人都可以（滑点上限更严）"
          ],
          "signer": true
        },
        {
          "name": "fund",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  110,
                  100
                ]
              },
              {
                "kind": "account",
                "path": "fund.admin",
                "account": "fund"
              }
            ]
          },
          "relations": [
            "epochRecord"
          ]
        },
        {
          "name": "btcPriceUpdate",
          "optional": true
        },
        {
          "name": "cbbtcPriceUpdate",
          "optional": true
        },
        {
          "name": "market",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "market.authority",
                "account": "mockMarket"
              }
            ]
          },
          "relations": [
            "fund"
          ]
        },
        {
          "name": "marketUsdc",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  117,
                  115,
                  100,
                  99
                ]
              },
              {
                "kind": "account",
                "path": "market"
              }
            ]
          }
        },
        {
          "name": "marketBtc",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  98,
                  116,
                  99
                ]
              },
              {
                "kind": "account",
                "path": "market"
              }
            ]
          }
        },
        {
          "name": "usdcVault",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  117,
                  115,
                  100,
                  99,
                  95,
                  118,
                  97,
                  117,
                  108,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              }
            ]
          }
        },
        {
          "name": "btcVault",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  98,
                  116,
                  99,
                  95,
                  118,
                  97,
                  117,
                  108,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              }
            ]
          }
        },
        {
          "name": "payoutVault",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  112,
                  97,
                  121,
                  111,
                  117,
                  116,
                  95,
                  118,
                  97,
                  117,
                  108,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              }
            ]
          }
        },
        {
          "name": "subscriptionEscrow",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  115,
                  117,
                  98,
                  115,
                  99,
                  114,
                  105,
                  112,
                  116,
                  105,
                  111,
                  110,
                  95,
                  101,
                  115,
                  99,
                  114,
                  111,
                  119
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              }
            ]
          }
        },
        {
          "name": "shareEscrow",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  115,
                  104,
                  97,
                  114,
                  101,
                  95,
                  101,
                  115,
                  99,
                  114,
                  111,
                  119
                ]
              },
              {
                "kind": "account",
                "path": "fund"
              }
            ]
          }
        },
        {
          "name": "shareMint",
          "writable": true
        },
        {
          "name": "epochRecord",
          "docs": [
            "最早一期未卖完的赎回资产的记录，没有时传最早一期待成交申购的记录（见 trade）。",
            "期号由 fund-logic 校验，这里校验它属于这个基金。"
          ],
          "writable": true,
          "optional": true
        },
        {
          "name": "tokenProgram",
          "address": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        }
      ],
      "args": []
    }
  ],
  "accounts": [
    {
      "name": "epochRecord",
      "discriminator": [
        193,
        72,
        26,
        28,
        161,
        98,
        58,
        132
      ]
    },
    {
      "name": "fund",
      "discriminator": [
        62,
        128,
        183,
        208,
        91,
        31,
        212,
        209
      ]
    },
    {
      "name": "mockMarket",
      "discriminator": [
        216,
        212,
        79,
        64,
        137,
        169,
        95,
        64
      ]
    },
    {
      "name": "ticket",
      "discriminator": [
        41,
        228,
        24,
        165,
        78,
        90,
        235,
        200
      ]
    }
  ],
  "events": [
    {
      "name": "configApplied",
      "discriminator": [
        193,
        101,
        90,
        205,
        113,
        125,
        184,
        13
      ]
    },
    {
      "name": "configProposed",
      "discriminator": [
        148,
        34,
        23,
        129,
        45,
        161,
        66,
        192
      ]
    },
    {
      "name": "feesCollected",
      "discriminator": [
        233,
        23,
        117,
        225,
        107,
        178,
        254,
        8
      ]
    },
    {
      "name": "fixed",
      "discriminator": [
        35,
        148,
        241,
        77,
        204,
        20,
        179,
        98
      ]
    },
    {
      "name": "priceBackfilled",
      "discriminator": [
        141,
        121,
        119,
        21,
        87,
        45,
        6,
        204
      ]
    },
    {
      "name": "subscriptionPauseChanged",
      "discriminator": [
        188,
        28,
        137,
        5,
        154,
        34,
        218,
        121
      ]
    },
    {
      "name": "traded",
      "discriminator": [
        225,
        202,
        73,
        175,
        147,
        43,
        160,
        150
      ]
    }
  ],
  "errors": [
    {
      "code": 6000,
      "name": "tooEarly",
      "msg": "还没到今天的定价时刻（UTC 0 点 + 设定的偏移）"
    },
    {
      "code": 6001,
      "name": "alreadySettled",
      "msg": ""
    },
    {
      "code": 6002,
      "name": "badPrice",
      "msg": ""
    },
    {
      "code": 6003,
      "name": "stalePrice",
      "msg": ""
    },
    {
      "code": 6004,
      "name": "slippageExceeded",
      "msg": ""
    },
    {
      "code": 6005,
      "name": "invalidConfig",
      "msg": ""
    },
    {
      "code": 6006,
      "name": "overflow",
      "msg": ""
    },
    {
      "code": 6007,
      "name": "nothingToClaim",
      "msg": ""
    },
    {
      "code": 6008,
      "name": "epochNotSettled",
      "msg": ""
    },
    {
      "code": 6009,
      "name": "belowMinSubscription",
      "msg": ""
    },
    {
      "code": 6010,
      "name": "zeroAmount",
      "msg": ""
    },
    {
      "code": 6011,
      "name": "tooManyPrices",
      "msg": ""
    },
    {
      "code": 6012,
      "name": "alreadyStarted",
      "msg": "基金已经开始运行，不能再写入历史价格"
    },
    {
      "code": 6013,
      "name": "wrongDecimals",
      "msg": "代币精度不对：USDC 应为 6 位，BTC 应为 8 位"
    },
    {
      "code": 6014,
      "name": "wrongMint",
      "msg": ""
    },
    {
      "code": 6015,
      "name": "notOwner",
      "msg": ""
    },
    {
      "code": 6016,
      "name": "priceNotAtFixTime",
      "msg": ""
    },
    {
      "code": 6017,
      "name": "nothingToTrade",
      "msg": ""
    },
    {
      "code": 6018,
      "name": "tradeTooSoon",
      "msg": ""
    },
    {
      "code": 6019,
      "name": "depegBlocksBuy",
      "msg": "cbBTC 脱锚，暂停买入"
    },
    {
      "code": 6020,
      "name": "sliceNotSold",
      "msg": ""
    },
    {
      "code": 6021,
      "name": "wrongEpochRecord",
      "msg": ""
    },
    {
      "code": 6022,
      "name": "tooManySlices",
      "msg": "未卖完的赎回资产太多，先卖出再定价"
    },
    {
      "code": 6023,
      "name": "fixPending",
      "msg": "已过今天的定价时刻、还没定价，基金调仓等待定价"
    },
    {
      "code": 6024,
      "name": "keeperOnly",
      "msg": ""
    },
    {
      "code": 6025,
      "name": "wrongEpoch",
      "msg": ""
    },
    {
      "code": 6026,
      "name": "tooLateToCancel",
      "msg": "已过关闭这一期的定价时刻，不能再撤销"
    },
    {
      "code": 6027,
      "name": "nothingToCancel",
      "msg": "排队记录是空的，没有可撤销的"
    },
    {
      "code": 6028,
      "name": "subscriptionsPaused",
      "msg": "申购已暂停（赎回不受影响）"
    },
    {
      "code": 6029,
      "name": "timelockNotElapsed",
      "msg": "配置提议还没过 48 小时时间锁"
    },
    {
      "code": 6030,
      "name": "noPendingConfig",
      "msg": ""
    },
    {
      "code": 6031,
      "name": "configFieldFixed",
      "msg": ""
    },
    {
      "code": 6032,
      "name": "priceAlreadyRecorded",
      "msg": ""
    },
    {
      "code": 6033,
      "name": "backfillOutOfRange",
      "msg": "只能回填过去、最近 64 天内的一天"
    },
    {
      "code": 6034,
      "name": "subscriptionCapExceeded",
      "msg": "超过申购上限（算上待成交的申购）"
    },
    {
      "code": 6035,
      "name": "priceTooUncertain",
      "msg": "Pyth 价格的置信区间太宽"
    },
    {
      "code": 6036,
      "name": "wrongPriceAccount",
      "msg": "不是有效的 Pyth 价格更新账户（所有者、验证级别或价格源不对）"
    }
  ],
  "types": [
    {
      "name": "configApplied",
      "type": {
        "kind": "struct",
        "fields": []
      }
    },
    {
      "name": "configData",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "window",
            "type": "u8"
          },
          {
            "name": "settleOffsetSecs",
            "type": "i64"
          },
          {
            "name": "maxSlippageBps",
            "type": "u16"
          },
          {
            "name": "subscriptionFeeBps",
            "type": "u16"
          },
          {
            "name": "minSubscription",
            "type": "u64"
          },
          {
            "name": "depegThresholdBps",
            "type": "u16"
          },
          {
            "name": "tradeCapUsdc",
            "type": "u64"
          },
          {
            "name": "minTradeIntervalSecs",
            "type": "i64"
          },
          {
            "name": "keeperWindowSecs",
            "type": "i64"
          },
          {
            "name": "publicSlippageBps",
            "type": "u16"
          },
          {
            "name": "withdrawalFeeBps",
            "type": "u16"
          },
          {
            "name": "subscriptionCapUsdc",
            "type": "u64"
          },
          {
            "name": "signalFeed",
            "type": {
              "defined": {
                "name": "signalFeedData"
              }
            }
          },
          {
            "name": "maxConfidenceBps",
            "type": "u16"
          },
          {
            "name": "maxStalenessSecs",
            "type": "i64"
          },
          {
            "name": "keeper",
            "type": "pubkey"
          },
          {
            "name": "feeAddress",
            "type": "pubkey"
          },
          {
            "name": "btcFeedId",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "cbbtcFeedId",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          }
        ]
      }
    },
    {
      "name": "configProposed",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "effectiveAt",
            "type": "i64"
          }
        ]
      }
    },
    {
      "name": "epochRecord",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "fund",
            "type": "pubkey"
          },
          {
            "name": "epoch",
            "type": "u64"
          },
          {
            "name": "firstEpoch",
            "type": "u64"
          },
          {
            "name": "signalPrice",
            "type": "u64"
          },
          {
            "name": "referencePrice",
            "type": "u64"
          },
          {
            "name": "movingAverage",
            "type": "u64"
          },
          {
            "name": "subscriptionUsdc",
            "type": "u64"
          },
          {
            "name": "mintedShares",
            "type": "u64"
          },
          {
            "name": "subscriptionFilled",
            "type": "bool"
          },
          {
            "name": "nettedUsdc",
            "type": "u64"
          },
          {
            "name": "nettedBtc",
            "type": "u64"
          },
          {
            "name": "refundUsdc",
            "type": "u64"
          },
          {
            "name": "redemptionShares",
            "type": "u64"
          },
          {
            "name": "sliceUsdc",
            "type": "u64"
          },
          {
            "name": "sliceBtc",
            "type": "u64"
          },
          {
            "name": "payoutUsdc",
            "type": "u64"
          },
          {
            "name": "sliceSoldOut",
            "type": "bool"
          },
          {
            "name": "targetBtc",
            "type": "bool"
          },
          {
            "name": "unclaimedSubscriptionUsdc",
            "type": "u64"
          },
          {
            "name": "unclaimedShares",
            "type": "u64"
          },
          {
            "name": "unclaimedRedemptionShares",
            "type": "u64"
          },
          {
            "name": "unclaimedPayoutUsdc",
            "type": "u64"
          },
          {
            "name": "unclaimedRefundUsdc",
            "type": "u64"
          },
          {
            "name": "cancelledSubscriptionUsdc",
            "type": "u64"
          },
          {
            "name": "cancelledShares",
            "type": "u64"
          },
          {
            "name": "fundUsdc",
            "type": "u64"
          },
          {
            "name": "fundBtc",
            "type": "u64"
          },
          {
            "name": "fundShares",
            "type": "u64"
          },
          {
            "name": "bump",
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "feesCollected",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "usdc",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "fixed",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "epoch",
            "type": "u64"
          },
          {
            "name": "firstEpoch",
            "type": "u64"
          },
          {
            "name": "signalPrice",
            "type": "u64"
          },
          {
            "name": "referencePrice",
            "type": "u64"
          },
          {
            "name": "movingAverage",
            "type": "u64"
          },
          {
            "name": "targetBtc",
            "type": "bool"
          }
        ]
      }
    },
    {
      "name": "fund",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "admin",
            "type": "pubkey"
          },
          {
            "name": "usdcMint",
            "type": "pubkey"
          },
          {
            "name": "btcMint",
            "type": "pubkey"
          },
          {
            "name": "shareMint",
            "type": "pubkey"
          },
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "config",
            "type": {
              "defined": {
                "name": "configData"
              }
            }
          },
          {
            "name": "state",
            "type": {
              "defined": {
                "name": "stateData"
              }
            }
          },
          {
            "name": "bump",
            "type": "u8"
          },
          {
            "name": "pendingConfig",
            "type": {
              "option": {
                "defined": {
                  "name": "pendingConfig"
                }
              }
            }
          }
        ]
      }
    },
    {
      "name": "mockMarket",
      "docs": [
        "原型专用：模拟交易所，按 Pyth 价格成交任意数量（以注入的流动性为限），收 fee_bps 手续费"
      ],
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "authority",
            "type": "pubkey"
          },
          {
            "name": "usdcMint",
            "type": "pubkey"
          },
          {
            "name": "btcMint",
            "type": "pubkey"
          },
          {
            "name": "feeBps",
            "type": "u16"
          },
          {
            "name": "bump",
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "pendingConfig",
      "docs": [
        "还没生效的配置提议"
      ],
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "config",
            "type": {
              "defined": {
                "name": "configData"
              }
            }
          },
          {
            "name": "effectiveAt",
            "type": "i64"
          }
        ]
      }
    },
    {
      "name": "pendingData",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "epoch",
            "type": "u64"
          },
          {
            "name": "usdc",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "priceBackfilled",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "day",
            "type": "i64"
          },
          {
            "name": "signalPrice",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "signalFeedData",
      "docs": [
        "信号价格源，对应 fund_logic::SignalFeed"
      ],
      "type": {
        "kind": "enum",
        "variants": [
          {
            "name": "cbBtc"
          },
          {
            "name": "btc"
          }
        ]
      }
    },
    {
      "name": "sliceData",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "epoch",
            "type": "u64"
          },
          {
            "name": "btc",
            "type": "u64"
          },
          {
            "name": "usdc",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "stateData",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "usdc",
            "type": "u64"
          },
          {
            "name": "btc",
            "type": "u64"
          },
          {
            "name": "shareSupply",
            "type": "u64"
          },
          {
            "name": "queuedSubscriptionUsdc",
            "type": "u64"
          },
          {
            "name": "queuedRedemptionShares",
            "type": "u64"
          },
          {
            "name": "nextEpoch",
            "type": "u64"
          },
          {
            "name": "nextSubscriptionUsdc",
            "type": "u64"
          },
          {
            "name": "nextRedemptionShares",
            "type": "u64"
          },
          {
            "name": "prices",
            "type": {
              "array": [
                "u64",
                64
              ]
            }
          },
          {
            "name": "priceDays",
            "type": {
              "array": [
                "i64",
                64
              ]
            }
          },
          {
            "name": "targetBtc",
            "type": "bool"
          },
          {
            "name": "lastFixDay",
            "type": "i64"
          },
          {
            "name": "lastTradeTime",
            "type": "i64"
          },
          {
            "name": "slices",
            "type": {
              "array": [
                {
                  "defined": {
                    "name": "sliceData"
                  }
                },
                8
              ]
            }
          },
          {
            "name": "sliceCount",
            "type": "u8"
          },
          {
            "name": "pending",
            "type": {
              "array": [
                {
                  "defined": {
                    "name": "pendingData"
                  }
                },
                8
              ]
            }
          },
          {
            "name": "pendingCount",
            "type": "u8"
          },
          {
            "name": "subscriptionsPaused",
            "type": "bool"
          },
          {
            "name": "feesOwed",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "subscriptionPauseChanged",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "paused",
            "type": "bool"
          }
        ]
      }
    },
    {
      "name": "ticket",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "fund",
            "type": "pubkey"
          },
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "epoch",
            "type": "u64"
          },
          {
            "name": "subscriptionUsdc",
            "type": "u64"
          },
          {
            "name": "redemptionShares",
            "type": "u64"
          },
          {
            "name": "sharesClaimed",
            "type": "u64"
          },
          {
            "name": "bump",
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "tradeSide",
      "type": {
        "kind": "enum",
        "variants": [
          {
            "name": "buyBtc"
          },
          {
            "name": "sellBtc"
          },
          {
            "name": "withoutTrade"
          }
        ]
      }
    },
    {
      "name": "traded",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "side",
            "type": {
              "defined": {
                "name": "tradeSide"
              }
            }
          },
          {
            "name": "amountIn",
            "type": "u64"
          },
          {
            "name": "amountOut",
            "type": "u64"
          },
          {
            "name": "recordEpoch",
            "type": {
              "option": "u64"
            }
          },
          {
            "name": "mintedShares",
            "type": "u64"
          }
        ]
      }
    }
  ]
};
