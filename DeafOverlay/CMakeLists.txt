# Subproject builds our overlay exe
add_executable(DeafOverlay
    DeafOverlay.cpp  # put the WinMain here (see sample below)
    # DeafOverlay.h  # optional
)

# C++20 and warnings
set_target_properties(DeafOverlay PROPERTIES
    CXX_STANDARD 20
    CXX_STANDARD_REQUIRED YES
)

if (MSVC)
  target_compile_options(DeafOverlay PRIVATE /W4 /permissive- /Zc:preprocessor)
endif()

# Link lightweight Win32 libs we need
target_link_libraries(DeafOverlay PRIVATE
    user32.lib gdi32.lib dwmapi.lib msimg32.lib
)

# Make it a GUI app (no console window) on MSVC
if (MSVC)
  target_link_options(DeafOverlay PRIVATE /SUBSYSTEM:WINDOWS)
endif()
