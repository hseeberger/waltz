window.BENCHMARK_DATA = {
  "lastUpdate": 1786982284583,
  "repoUrl": "https://github.com/hseeberger/waltz",
  "entries": {
    "Benchmark": [
      {
        "commit": {
          "author": {
            "email": "git@heikoseeberger.de",
            "name": "Heiko Seeberger",
            "username": "hseeberger"
          },
          "committer": {
            "email": "git@heikoseeberger.de",
            "name": "Heiko Seeberger",
            "username": "hseeberger"
          },
          "distinct": true,
          "id": "05df542a00f7bb5d9375159fd3347c21fb5a480e",
          "message": "feat: core actors",
          "timestamp": "2026-08-04T09:12:29+02:00",
          "tree_id": "da7e3dbf4eea51f3f206f755d4c6e0138a90c34a",
          "url": "https://github.com/hseeberger/waltz/commit/05df542a00f7bb5d9375159fd3347c21fb5a480e"
        },
        "date": 1785827776308,
        "tool": "cargo",
        "benches": [
          {
            "name": "flood/unbounded",
            "value": 8688260,
            "range": "± 234712",
            "unit": "ns/iter"
          },
          {
            "name": "flood/bounded",
            "value": 10208552,
            "range": "± 50651",
            "unit": "ns/iter"
          },
          {
            "name": "ping_pong/pairs/1",
            "value": 529817,
            "range": "± 3360",
            "unit": "ns/iter"
          },
          {
            "name": "ping_pong/pairs/4",
            "value": 649691,
            "range": "± 4530",
            "unit": "ns/iter"
          },
          {
            "name": "fan_out/workers/4",
            "value": 4286942,
            "range": "± 142188",
            "unit": "ns/iter"
          },
          {
            "name": "fan_out/workers/16",
            "value": 2801423,
            "range": "± 15506",
            "unit": "ns/iter"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "git@heikoseeberger.de",
            "name": "Heiko Seeberger",
            "username": "hseeberger"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "e4055012ebc7383341076b0b6b2ab00544ae7517",
          "message": "Merge pull request #5 from hseeberger/dependabot/github_actions/dtolnay/rust-toolchain-6c977a6ca4077a0ceb28ffbe03f59d46e9ac8772\n\nci(deps): bump dtolnay/rust-toolchain from 2c7215f132e9ebf062739d9130488b56d53c060c to 6c977a6ca4077a0ceb28ffbe03f59d46e9ac8772",
          "timestamp": "2026-08-11T22:43:35+02:00",
          "tree_id": "ef8184c1fd292ef7b588f37b7978888cb50c3f0a",
          "url": "https://github.com/hseeberger/waltz/commit/e4055012ebc7383341076b0b6b2ab00544ae7517"
        },
        "date": 1786481129438,
        "tool": "cargo",
        "benches": [
          {
            "name": "flood/unbounded",
            "value": 10048754,
            "range": "± 222475",
            "unit": "ns/iter"
          },
          {
            "name": "flood/bounded",
            "value": 11621705,
            "range": "± 270313",
            "unit": "ns/iter"
          },
          {
            "name": "ping_pong/pairs/1",
            "value": 667412,
            "range": "± 10937",
            "unit": "ns/iter"
          },
          {
            "name": "ping_pong/pairs/4",
            "value": 802912,
            "range": "± 3517",
            "unit": "ns/iter"
          },
          {
            "name": "fan_out/workers/4",
            "value": 4875306,
            "range": "± 80126",
            "unit": "ns/iter"
          },
          {
            "name": "fan_out/workers/16",
            "value": 3496353,
            "range": "± 19516",
            "unit": "ns/iter"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "git@heikoseeberger.de",
            "name": "Heiko Seeberger",
            "username": "hseeberger"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "02b5aef63ab08d435f7c413cbebcce78d1c3ed56",
          "message": "Merge pull request #6 from hseeberger/fix/restart-parent-stop\n\nfix: honor parent stop arriving during a restart's stop_children",
          "timestamp": "2026-08-12T19:09:15+02:00",
          "tree_id": "50179a72b4143818212566f935abcb808bc09d67",
          "url": "https://github.com/hseeberger/waltz/commit/02b5aef63ab08d435f7c413cbebcce78d1c3ed56"
        },
        "date": 1786554710592,
        "tool": "cargo",
        "benches": [
          {
            "name": "flood/unbounded",
            "value": 9339576,
            "range": "± 107836",
            "unit": "ns/iter"
          },
          {
            "name": "flood/bounded",
            "value": 11063125,
            "range": "± 47616",
            "unit": "ns/iter"
          },
          {
            "name": "ping_pong/pairs/1",
            "value": 705375,
            "range": "± 11334",
            "unit": "ns/iter"
          },
          {
            "name": "ping_pong/pairs/4",
            "value": 844162,
            "range": "± 35871",
            "unit": "ns/iter"
          },
          {
            "name": "fan_out/workers/4",
            "value": 4581980,
            "range": "± 130623",
            "unit": "ns/iter"
          },
          {
            "name": "fan_out/workers/16",
            "value": 3452113,
            "range": "± 21623",
            "unit": "ns/iter"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "git@heikoseeberger.de",
            "name": "Heiko Seeberger",
            "username": "hseeberger"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "dc61901d5545ef2e6c8d925010843b3f5acb82af",
          "message": "Merge pull request #7 from hseeberger/fix/terminate-mailbox-close\n\nfix: report termination instead of full mailbox while terminating",
          "timestamp": "2026-08-12T19:13:14+02:00",
          "tree_id": "525201746350610055103a13184da767505f21f9",
          "url": "https://github.com/hseeberger/waltz/commit/dc61901d5545ef2e6c8d925010843b3f5acb82af"
        },
        "date": 1786554881889,
        "tool": "cargo",
        "benches": [
          {
            "name": "flood/unbounded",
            "value": 11396819,
            "range": "± 783800",
            "unit": "ns/iter"
          },
          {
            "name": "flood/bounded",
            "value": 11251209,
            "range": "± 71701",
            "unit": "ns/iter"
          },
          {
            "name": "ping_pong/pairs/1",
            "value": 741884,
            "range": "± 4367",
            "unit": "ns/iter"
          },
          {
            "name": "ping_pong/pairs/4",
            "value": 791497,
            "range": "± 3795",
            "unit": "ns/iter"
          },
          {
            "name": "fan_out/workers/4",
            "value": 5265101,
            "range": "± 274125",
            "unit": "ns/iter"
          },
          {
            "name": "fan_out/workers/16",
            "value": 3629500,
            "range": "± 34082",
            "unit": "ns/iter"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "git@heikoseeberger.de",
            "name": "Heiko Seeberger",
            "username": "hseeberger"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "c95605204e3e090f688461a037700b3a6a3f0bee",
          "message": "Merge pull request #8 from hseeberger/refactor/tracing\n\nrefactor: switch from log/logforth to tracing",
          "timestamp": "2026-08-13T14:19:45+02:00",
          "tree_id": "7780ce4059a55f20eec8470f89fdd7018b1e2f63",
          "url": "https://github.com/hseeberger/waltz/commit/c95605204e3e090f688461a037700b3a6a3f0bee"
        },
        "date": 1786623676856,
        "tool": "cargo",
        "benches": [
          {
            "name": "flood/unbounded",
            "value": 10150389,
            "range": "± 90813",
            "unit": "ns/iter"
          },
          {
            "name": "flood/bounded",
            "value": 13279544,
            "range": "± 46327",
            "unit": "ns/iter"
          },
          {
            "name": "ping_pong/pairs/1",
            "value": 686721,
            "range": "± 13294",
            "unit": "ns/iter"
          },
          {
            "name": "ping_pong/pairs/4",
            "value": 890027,
            "range": "± 24433",
            "unit": "ns/iter"
          },
          {
            "name": "fan_out/workers/4",
            "value": 4786653,
            "range": "± 167024",
            "unit": "ns/iter"
          },
          {
            "name": "fan_out/workers/16",
            "value": 3601391,
            "range": "± 18265",
            "unit": "ns/iter"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "git@heikoseeberger.de",
            "name": "Heiko Seeberger",
            "username": "hseeberger"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "e2e737502f5e5c0d55d950b8126a62c6c81e2f16",
          "message": "Merge pull request #9 from hseeberger/fix/ping-pong-teardown\n\nfix: align ping-pong teardown across benchmarked frameworks",
          "timestamp": "2026-08-13T17:34:45+02:00",
          "tree_id": "f463044c87f9059b11e52506c6773482559be82d",
          "url": "https://github.com/hseeberger/waltz/commit/e2e737502f5e5c0d55d950b8126a62c6c81e2f16"
        },
        "date": 1786635375192,
        "tool": "cargo",
        "benches": [
          {
            "name": "flood/unbounded",
            "value": 10272962,
            "range": "± 175727",
            "unit": "ns/iter"
          },
          {
            "name": "flood/bounded",
            "value": 10897385,
            "range": "± 114524",
            "unit": "ns/iter"
          },
          {
            "name": "ping_pong/pairs/1",
            "value": 687380,
            "range": "± 12764",
            "unit": "ns/iter"
          },
          {
            "name": "ping_pong/pairs/4",
            "value": 811837,
            "range": "± 5058",
            "unit": "ns/iter"
          },
          {
            "name": "fan_out/workers/4",
            "value": 4927222,
            "range": "± 140470",
            "unit": "ns/iter"
          },
          {
            "name": "fan_out/workers/16",
            "value": 3585237,
            "range": "± 22544",
            "unit": "ns/iter"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "git@heikoseeberger.de",
            "name": "Heiko Seeberger",
            "username": "hseeberger"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "19aa4bdd2419d685e972759cf533edc1c373f54a",
          "message": "Merge pull request #10 from hseeberger/fix/config-deny-unknown-fields\n\nfix: reject unknown fields in config deserialization",
          "timestamp": "2026-08-13T17:40:00+02:00",
          "tree_id": "f7d255131082274fba726f916b1b273c72869d4f",
          "url": "https://github.com/hseeberger/waltz/commit/19aa4bdd2419d685e972759cf533edc1c373f54a"
        },
        "date": 1786635691956,
        "tool": "cargo",
        "benches": [
          {
            "name": "flood/unbounded",
            "value": 10205639,
            "range": "± 232672",
            "unit": "ns/iter"
          },
          {
            "name": "flood/bounded",
            "value": 10528586,
            "range": "± 186258",
            "unit": "ns/iter"
          },
          {
            "name": "ping_pong/pairs/1",
            "value": 650282,
            "range": "± 13342",
            "unit": "ns/iter"
          },
          {
            "name": "ping_pong/pairs/4",
            "value": 798400,
            "range": "± 18215",
            "unit": "ns/iter"
          },
          {
            "name": "fan_out/workers/4",
            "value": 4803624,
            "range": "± 80514",
            "unit": "ns/iter"
          },
          {
            "name": "fan_out/workers/16",
            "value": 3597178,
            "range": "± 9770",
            "unit": "ns/iter"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "git@heikoseeberger.de",
            "name": "Heiko Seeberger",
            "username": "hseeberger"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "b3e0108292cf9ad3cf332eb240c6807ca7c3495d",
          "message": "Merge pull request #11 from hseeberger/refactor/backoff-default-via-new\n\nrefactor: route Backoff::default through the validating constructor",
          "timestamp": "2026-08-13T17:44:58+02:00",
          "tree_id": "fae716941c71a9441102bf4da82473b34d009454",
          "url": "https://github.com/hseeberger/waltz/commit/b3e0108292cf9ad3cf332eb240c6807ca7c3495d"
        },
        "date": 1786635984797,
        "tool": "cargo",
        "benches": [
          {
            "name": "flood/unbounded",
            "value": 20953680,
            "range": "± 886419",
            "unit": "ns/iter"
          },
          {
            "name": "flood/bounded",
            "value": 28390899,
            "range": "± 1002205",
            "unit": "ns/iter"
          },
          {
            "name": "ping_pong/pairs/1",
            "value": 824778,
            "range": "± 12886",
            "unit": "ns/iter"
          },
          {
            "name": "ping_pong/pairs/4",
            "value": 1130867,
            "range": "± 13882",
            "unit": "ns/iter"
          },
          {
            "name": "fan_out/workers/4",
            "value": 9267062,
            "range": "± 439246",
            "unit": "ns/iter"
          },
          {
            "name": "fan_out/workers/16",
            "value": 8367919,
            "range": "± 610665",
            "unit": "ns/iter"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "git@heikoseeberger.de",
            "name": "Heiko Seeberger",
            "username": "hseeberger"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "37f2f8ffcea86c05f9fdc2a1648f349e3464bc39",
          "message": "Merge pull request #12 from hseeberger/refactor/take-watchers-one-shot\n\nrefactor: make ClosedMailbox::take_watchers consume the mailbox",
          "timestamp": "2026-08-13T17:49:06+02:00",
          "tree_id": "0aa0e3fca7974f50a4648212427eaa1ff8de0713",
          "url": "https://github.com/hseeberger/waltz/commit/37f2f8ffcea86c05f9fdc2a1648f349e3464bc39"
        },
        "date": 1786636233657,
        "tool": "cargo",
        "benches": [
          {
            "name": "flood/unbounded",
            "value": 11557229,
            "range": "± 104098",
            "unit": "ns/iter"
          },
          {
            "name": "flood/bounded",
            "value": 13613910,
            "range": "± 132750",
            "unit": "ns/iter"
          },
          {
            "name": "ping_pong/pairs/1",
            "value": 779662,
            "range": "± 8600",
            "unit": "ns/iter"
          },
          {
            "name": "ping_pong/pairs/4",
            "value": 779656,
            "range": "± 4168",
            "unit": "ns/iter"
          },
          {
            "name": "fan_out/workers/4",
            "value": 5945443,
            "range": "± 131988",
            "unit": "ns/iter"
          },
          {
            "name": "fan_out/workers/16",
            "value": 3625175,
            "range": "± 20652",
            "unit": "ns/iter"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "git@heikoseeberger.de",
            "name": "Heiko Seeberger",
            "username": "hseeberger"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "0b0126008773dc4a47c4438320d6d791f4f8da3b",
          "message": "Merge pull request #13 from hseeberger/test/core-coverage-gaps\n\ntest: close coverage gaps in quota, termination and watch tests",
          "timestamp": "2026-08-13T17:53:01+02:00",
          "tree_id": "7719c57add6af08d60179a90b30f9e35d1a7f91f",
          "url": "https://github.com/hseeberger/waltz/commit/0b0126008773dc4a47c4438320d6d791f4f8da3b"
        },
        "date": 1786636462636,
        "tool": "cargo",
        "benches": [
          {
            "name": "flood/unbounded",
            "value": 11550278,
            "range": "± 188505",
            "unit": "ns/iter"
          },
          {
            "name": "flood/bounded",
            "value": 11847611,
            "range": "± 338843",
            "unit": "ns/iter"
          },
          {
            "name": "ping_pong/pairs/1",
            "value": 698610,
            "range": "± 16058",
            "unit": "ns/iter"
          },
          {
            "name": "ping_pong/pairs/4",
            "value": 780918,
            "range": "± 5529",
            "unit": "ns/iter"
          },
          {
            "name": "fan_out/workers/4",
            "value": 5620216,
            "range": "± 105836",
            "unit": "ns/iter"
          },
          {
            "name": "fan_out/workers/16",
            "value": 3630203,
            "range": "± 24386",
            "unit": "ns/iter"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "git@heikoseeberger.de",
            "name": "Heiko Seeberger",
            "username": "hseeberger"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "10612ad1f63f8971a34a99f0a881a5237c4facb0",
          "message": "Merge pull request #14 from hseeberger/fix/scatter-gather-unreachable-message\n\nfix: correct misleading unreachable message in scatter_gather",
          "timestamp": "2026-08-13T17:56:46+02:00",
          "tree_id": "b4f596d1cbc23d6b754848dec2218e88bd675058",
          "url": "https://github.com/hseeberger/waltz/commit/10612ad1f63f8971a34a99f0a881a5237c4facb0"
        },
        "date": 1786636693579,
        "tool": "cargo",
        "benches": [
          {
            "name": "flood/unbounded",
            "value": 9569862,
            "range": "± 274212",
            "unit": "ns/iter"
          },
          {
            "name": "flood/bounded",
            "value": 14211817,
            "range": "± 87446",
            "unit": "ns/iter"
          },
          {
            "name": "ping_pong/pairs/1",
            "value": 685025,
            "range": "± 10942",
            "unit": "ns/iter"
          },
          {
            "name": "ping_pong/pairs/4",
            "value": 793304,
            "range": "± 3811",
            "unit": "ns/iter"
          },
          {
            "name": "fan_out/workers/4",
            "value": 4763034,
            "range": "± 148352",
            "unit": "ns/iter"
          },
          {
            "name": "fan_out/workers/16",
            "value": 3464576,
            "range": "± 18890",
            "unit": "ns/iter"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "git@heikoseeberger.de",
            "name": "Heiko Seeberger",
            "username": "hseeberger"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "f865e34b75d1c204b108fa7056cb8ac9bd54d69f",
          "message": "Merge pull request #15 from hseeberger/feat/request-response\n\nfeat: add request-response via ask and reply_to",
          "timestamp": "2026-08-15T13:56:30+02:00",
          "tree_id": "01c6b2ab391bad5ddfa45b8a33b49c7e99e496c5",
          "url": "https://github.com/hseeberger/waltz/commit/f865e34b75d1c204b108fa7056cb8ac9bd54d69f"
        },
        "date": 1786795074245,
        "tool": "cargo",
        "benches": [
          {
            "name": "flood/unbounded",
            "value": 10509439,
            "range": "± 215266",
            "unit": "ns/iter"
          },
          {
            "name": "flood/bounded",
            "value": 11709503,
            "range": "± 54550",
            "unit": "ns/iter"
          },
          {
            "name": "ping_pong/pairs/1",
            "value": 695566,
            "range": "± 18476",
            "unit": "ns/iter"
          },
          {
            "name": "ping_pong/pairs/4",
            "value": 812427,
            "range": "± 23575",
            "unit": "ns/iter"
          },
          {
            "name": "fan_out/workers/4",
            "value": 4998894,
            "range": "± 130243",
            "unit": "ns/iter"
          },
          {
            "name": "fan_out/workers/16",
            "value": 3544978,
            "range": "± 16802",
            "unit": "ns/iter"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "git@heikoseeberger.de",
            "name": "Heiko Seeberger",
            "username": "hseeberger"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "5c7a1501e4aaea85adfe0493df142a1f82513c64",
          "message": "Merge pull request #17 from hseeberger/feat/config-setters\n\nfeat: add with_* setters to ActorConfig and RestartPolicy",
          "timestamp": "2026-08-17T17:55:19+02:00",
          "tree_id": "2a543c7ac7a6e6df12f0b33aa03831884218864b",
          "url": "https://github.com/hseeberger/waltz/commit/5c7a1501e4aaea85adfe0493df142a1f82513c64"
        },
        "date": 1786982283429,
        "tool": "cargo",
        "benches": [
          {
            "name": "flood/unbounded",
            "value": 9585400,
            "range": "± 226338",
            "unit": "ns/iter"
          },
          {
            "name": "flood/bounded",
            "value": 14859562,
            "range": "± 83405",
            "unit": "ns/iter"
          },
          {
            "name": "ping_pong/pairs/1",
            "value": 663907,
            "range": "± 8406",
            "unit": "ns/iter"
          },
          {
            "name": "ping_pong/pairs/4",
            "value": 799483,
            "range": "± 67899",
            "unit": "ns/iter"
          },
          {
            "name": "fan_out/workers/4",
            "value": 4914332,
            "range": "± 128004",
            "unit": "ns/iter"
          },
          {
            "name": "fan_out/workers/16",
            "value": 3515752,
            "range": "± 14029",
            "unit": "ns/iter"
          }
        ]
      }
    ]
  }
}