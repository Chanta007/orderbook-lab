#pragma once
// WALKTHROUGH, msg.hpp
//
// This is the only data type that crosses a process boundary. Python packs
// these bytes. feedd writes the same bytes into the ring and the WAL. The
// TUI reads them back. If you change a field, change python/feed_adapter.py
// MSG_FMT in the same commit. static_assert locks the size at 72.
//
// How to explain one record:
//   magic    ASCII "OBLB". A reader rejects anything else.
//   nbytes   always 72. Guards against a short read.
//   type     Depth updates one price. DepthReset clears one whole side
//            before the new prices of a Binance top-of-book picture.
//            Heartbeat and Cmd are reserved and ignored by the book.
//   side     0 bid, 1 ask. Both exists so a later command can name a side
//            without overloading bid/ask.
//   seq      filled by the ring when the message is published, not by Python.
//   ts_ns    filled by feedd. Not exchange time.
//   trace_id 16 bytes, span_id 8 bytes. Same widths as W3C trace context.
//            Filled locally. No collector.
//   px_e8    price times 100,000,000. 65000.10 dollars is 6500010000000.
//            Integers so two prices compare without floating point.
//   qty_e8   size, same scale. Zero on a Depth message deletes that price.
//   symbol   8 bytes, not a C string you can trust past 7 characters plus NUL.
#include <cstdint>
#include <cstring>

namespace ob {

inline constexpr uint32_t kMagic = 0x4F424C42u; // OBLB

enum class Type : uint8_t { Depth = 1, Heartbeat = 2, Cmd = 3, DepthReset = 4 };
enum class Side : uint8_t { Bid = 0, Ask = 1, Both = 2 };

#pragma pack(push, 1)
struct Msg {
  uint32_t magic;
  uint16_t nbytes;
  Type type;
  Side side;
  uint64_t seq;
  uint64_t ts_ns;
  uint8_t trace_id[16];
  uint8_t span_id[8];
  int64_t px_e8;
  int64_t qty_e8;
  char symbol[8];
};
#pragma pack(pop)

static_assert(sizeof(Msg) == 72, "Msg must stay packed 72 bytes");

inline void clear_msg(Msg& m) { std::memset(&m, 0, sizeof(m)); }

inline bool valid(const Msg& m) {
  return m.magic == kMagic && m.nbytes == sizeof(Msg);
}

} // namespace ob
