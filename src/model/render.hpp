#pragma once

#include "report.hpp"

#include <string>

namespace iat {

struct ExitDecision {
    int code = 5;
    std::string reason;
};

// Exit code from the report contents; priority 5 > 4 > 3 > 1 > 2 > 0.
ExitDecision decide_exit(const Report& r);

std::string render_markdown(const Report& r);

// Escape Markdown control characters and limit length (for names/metadata).
std::string md_escape(const std::string& s, size_t max_chars = 200);

}  // namespace iat
