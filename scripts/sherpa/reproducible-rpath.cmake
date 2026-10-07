# Upstream enables CMAKE_INSTALL_RPATH_USE_LINK_PATH after project(). Disable
# it on the final shared targets before generation, without patching upstream.
# Removing an absolute RPATH with install_name_tool after linking is too late:
# it has already affected section offsets, LC_UUID, and the ad-hoc signature.
if(CMAKE_CURRENT_SOURCE_DIR STREQUAL CMAKE_SOURCE_DIR)
  function(seasnail_finalize_native_rpaths)
    foreach(native_target sherpa-onnx-c-api sherpa-onnx-cxx-api)
      if(TARGET "${native_target}")
        set_target_properties("${native_target}" PROPERTIES
          INSTALL_RPATH_USE_LINK_PATH FALSE
          BUILD_WITH_INSTALL_RPATH TRUE
          INSTALL_RPATH "@loader_path")
      endif()
    endforeach()
  endfunction()
  cmake_language(DEFER CALL seasnail_finalize_native_rpaths)
endif()
