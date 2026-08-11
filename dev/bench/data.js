window.BENCHMARK_DATA = {
  "lastUpdate": 1786481129905,
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
      }
    ]
  }
}