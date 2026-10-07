#include "text_visibility.h"

#include <cassert>

int main() {
  using seasnail_sidecar::HasVisibleText;
  assert(!HasVisibleText(nullptr));
  assert(!HasVisibleText(""));
  assert(!HasVisibleText(" \t\r\n"));
  assert(!HasVisibleText("\xC2\xA0\xE2\x80\x83\xE3\x80\x80"));
  assert(HasVisibleText(" hello "));
  assert(HasVisibleText("\xE3\x80\x80\xE4\xBD\xA0\xE5\xA5\xBD"));
  assert(HasVisibleText("\xFF"));
}
