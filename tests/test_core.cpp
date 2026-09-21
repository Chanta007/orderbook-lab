#include "ob/book.hpp"
#include "ob/msg.hpp"
#include "ob/ring.hpp"
#include "ob/wal.hpp"
#include <cassert>
#include <cstdio>
#include <cstring>
#include <string>
#include <unistd.h>

static std::string tmp(const char* suffix) {
  char buf[128];
  std::snprintf(buf, sizeof(buf), "/tmp/obtest-%d-%s", static_cast<int>(getpid()), suffix);
  return buf;
}

int main() {
  ob::Msg m{};
  ob::clear_msg(m);
  m.magic = ob::kMagic;
  m.nbytes = sizeof(ob::Msg);
  m.type = ob::Type::Depth;
  m.side = ob::Side::Bid;
  m.px_e8 = 10000000000LL;
  m.qty_e8 = 150000000LL;
  std::strncpy(m.symbol, "BTCUSDT", 7);

  auto rp = tmp("bus");
  auto wp = tmp("wal");
  ::unlink(rp.c_str());
  ::unlink(wp.c_str());
  {
    ob::Ring ring(rp, true);
    ob::Wal wal(wp, true);
    ring.publish(m);
    wal.append(m);
    m.side = ob::Side::Ask;
    m.px_e8 = 10001000000LL;
    ring.publish(m);
    wal.append(m);
    wal.sync();
  }
  ob::Ring ring(rp, false);
  uint64_t c = 0;
  ob::Book book;
  ob::Msg out{};
  int n = 0;
  while (ring.consume(c, out)) {
    book.apply(out);
    ++n;
  }
  assert(n == 2);
  assert(book.bid_count() == 1);
  assert(book.ask_count() == 1);
  auto replay = ob::Wal::replay(wp);
  assert(replay.size() == 2);
  ::unlink(rp.c_str());
  ::unlink(wp.c_str());
  std::puts("ok");
  return 0;
}
