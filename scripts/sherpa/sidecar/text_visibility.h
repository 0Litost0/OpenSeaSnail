#pragma once

#include <string_view>

namespace seasnail_sidecar {

// Match the Unicode White_Space characters recognized by Rust's str::trim.
// Unknown or malformed UTF-8 remains visible so an invalid model result is
// never silently converted into a successful empty transcription.
inline bool HasVisibleText(const char *text) {
  if (!text) return false;
  constexpr std::string_view kWhitespace[] = {
      "\xC2\x85", "\xC2\xA0", "\xE1\x9A\x80", "\xE2\x80\x80", "\xE2\x80\x81",
      "\xE2\x80\x82", "\xE2\x80\x83", "\xE2\x80\x84", "\xE2\x80\x85", "\xE2\x80\x86",
      "\xE2\x80\x87", "\xE2\x80\x88", "\xE2\x80\x89", "\xE2\x80\x8A", "\xE2\x80\xA8",
      "\xE2\x80\xA9", "\xE2\x80\xAF", "\xE2\x81\x9F", "\xE3\x80\x80"};
  std::string_view remaining(text);
  while (!remaining.empty()) {
    const unsigned char first = static_cast<unsigned char>(remaining.front());
    if (first == ' ' || (first >= '\t' && first <= '\r')) {
      remaining.remove_prefix(1);
      continue;
    }
    bool matched = false;
    for (std::string_view whitespace : kWhitespace) {
      if (remaining.substr(0, whitespace.size()) == whitespace) {
        remaining.remove_prefix(whitespace.size());
        matched = true;
        break;
      }
    }
    if (!matched) return true;
  }
  return false;
}

}  // namespace seasnail_sidecar
