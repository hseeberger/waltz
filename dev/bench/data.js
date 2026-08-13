window.BENCHMARK_DATA = {
  "lastUpdate": 1786635376161,
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
      }
    ]
  }
}