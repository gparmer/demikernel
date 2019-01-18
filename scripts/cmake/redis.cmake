if(NOT REDIS_DOT_CMAKE_INCLUDED)
set(REDIS_DOT_CMAKE_INCLUDED YES)

include(ExternalProject)

# redis
function(add_redis REDIS_TARGET LIBOS_TARGET REDIS_SOURCE_DIR)
    set(REDIS_BINARY_DIR ${CMAKE_BINARY_DIR}/ExternalProject/${REDIS_TARGET})

    if(CMAKE_BUILD_TYPE MATCHES "Rel")
        set(OPT_CFLAGS -O3)
    else(CMAKE_BUILD_TYPE MATCHES "Rel")
        set(OPT_CFLAGS -O0)
    endif(CMAKE_BUILD_TYPE MATCHES "Rel")

    get_property(
        HOARD_TARGET
        TARGET ${LIBOS_TARGET}
        PROPERTY HOARD
    )
    if(DEFINED HOARD_TARGET)
        message("${REDIS_TARGET} => ${LIBOS_TARGET}:HOARD=${HOARD_TARGET}")
        ExternalProject_Get_Property(${HOARD_TARGET} SOURCE_DIR)
        set(DEMETER_MALLOC ${SOURCE_DIR}/src/libhoard.so)
    else(DEFINED HOARD_TARGET)
        set(DEMETER_MALLOC libc)
    endif(DEFINED HOARD_TARGET)

    ExternalProject_Add(${REDIS_TARGET}
        PREFIX ${REDIS_BINARY_DIR}
        SOURCE_DIR ${REDIS_SOURCE_DIR}
        CONFIGURE_COMMAND echo "No CONFIGURE_COMMAND for target `${REDIS_TARGET}`"
        BUILD_COMMAND make -C ${REDIS_SOURCE_DIR} PREFIX=${REDIS_BINARY_DIR} MALLOC=${DEMETER_MALLOC} DEMETER_INCLUDE=${CMAKE_SOURCE_DIR}/include DEMETER_LIBOS_SO=$<TARGET_FILE:${LIBOS_TARGET}> OPTIMIZATION=${OPT_CFLAGS} V=1
        INSTALL_COMMAND make -C ${REDIS_SOURCE_DIR} install PREFIX=${REDIS_BINARY_DIR} MALLOC=${DEMETER_MALLOC} DEMETER_INCLUDE=${CMAKE_SOURCE_DIR}/include DEMETER_LIBOS_SO=$<TARGET_FILE:${LIBOS_TARGET}> OPTIMIZATION=${OPT_CFLAGS} V=1
    )
endfunction(add_redis)

endif(NOT REDIS_DOT_CMAKE_INCLUDED)
