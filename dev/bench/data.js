window.BENCHMARK_DATA = {
  "lastUpdate": 1785827777071,
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
      }
    ]
  }
}