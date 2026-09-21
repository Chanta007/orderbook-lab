#pragma once
#include <cstdint>
#include <cstring>

namespace ob {

inline constexpr uint32_t kMagic = 0x4F424C42u; // OBLB

enum class Type : uint8_t { Depth = 1, Heartbeat = 2, Cmd = 3 };
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
