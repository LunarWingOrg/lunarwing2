;; Source-only MCP component fixture. Wasmtime compiles this component-model
;; text through the same Component::new path used for binary components.
(component
  (core module $server
    (memory (export "memory") 1)
    (global $next (mut i32) (i32.const 8192))
    (global $calls (mut i32) (i32.const 0))

    (data (i32.const 1024) "{\22jsonrpc\22:\222.0\22,\22id\22:1,\22result\22:{\22protocolVersion\22:\222024-11-05\22,\22serverInfo\22:{\22name\22:\22fixture\22,\22version\22:\221\22},\22capabilities\22:{\22tools\22:{}}}}")
    (data (i32.const 2048) "{\22jsonrpc\22:\222.0\22,\22id\22:2,\22result\22:{\22tools\22:[{\22name\22:\22alpha\22,\22description\22:\22page one\22,\22inputSchema\22:{\22type\22:\22object\22}}],\22nextCursor\22:\22page-2\22}}")
    (data (i32.const 3072) "{\22jsonrpc\22:\222.0\22,\22id\22:3,\22result\22:{\22tools\22:[{\22name\22:\22beta\22,\22description\22:\22page two\22,\22inputSchema\22:{\22type\22:\22object\22}}]}}")
    (data (i32.const 4096) "{\22jsonrpc\22:\222.0\22,\22id\22:4,\22result\22:{\22content\22:[{\22type\22:\22text\22,\22text\22:\22actor-state-retained\22}],\22is_error\22:false}}")
    (data (i32.const 4600) "{\22jsonrpc\22:\222.0\22,\22id\22:90,\22result\22:{}}")

    ;; Expected host envelopes. Byte-exact comparison makes this fixture fail
    ;; closed if method, id, params, protocol, capabilities, cursor, or arguments
    ;; change. In particular, notification has no id field.
    ;;
    ;; The initialize envelope is templated: the host substitutes the exact
    ;; bytes produced by McpRequest::initialize(1) at fixture-render time, so
    ;; the embedded clientInfo version can never drift from the crate version.
    (data (i32.const 5000) "__INITIALIZE_ENVELOPE__")
    (data (i32.const 5200) "{\22jsonrpc\22:\222.0\22,\22method\22:\22notifications/initialized\22}")
    (data (i32.const 5280) "{\22jsonrpc\22:\222.0\22,\22id\22:2,\22method\22:\22tools/list\22}")
    (data (i32.const 5360) "{\22jsonrpc\22:\222.0\22,\22id\22:3,\22method\22:\22tools/list\22,\22params\22:{\22cursor\22:\22page-2\22}}")
    (data (i32.const 5480) "{\22jsonrpc\22:\222.0\22,\22id\22:4,\22method\22:\22tools/call\22,\22params\22:{\22arguments\22:{\22value\22:1},\22name\22:\22alpha\22}}")
    (data (i32.const 5620) "{\22jsonrpc\22:\222.0\22,\22id\22:90,\22method\22:\22fixture/trap\22}")
    (data (i32.const 5700) "{\22jsonrpc\22:\222.0\22,\22id\22:90,\22method\22:\22fixture/timeout\22}")
    (data (i32.const 5790) "{\22jsonrpc\22:\222.0\22,\22id\22:90,\22method\22:\22fixture/memory\22}")

    (func (export "cabi_realloc")
      (param $old i32) (param $old-size i32) (param $align i32) (param $new-size i32)
      (result i32)
      (local $result i32)
      global.get $next
      local.tee $result
      local.get $new-size
      i32.add
      global.set $next
      local.get $result)

    (func $assert-request
      (param $actual i32) (param $actual-len i32)
      (param $expected i32) (param $expected-len i32)
      (local $offset i32)
      local.get $actual-len
      local.get $expected-len
      i32.ne
      if
        unreachable
      end
      loop $compare
        local.get $offset
        local.get $expected-len
        i32.lt_u
        if
          local.get $actual
          local.get $offset
          i32.add
          i32.load8_u
          local.get $expected
          local.get $offset
          i32.add
          i32.load8_u
          i32.ne
          if
            unreachable
          end
          local.get $offset
          i32.const 1
          i32.add
          local.set $offset
          br $compare
        end
      end)

    ;; Canonical ABI for handle(string) -> result<string, string>:
    ;; request pointer/length plus returned indirect-result pointer. Result
    ;; layout is discriminant, padding, string pointer, string length.
    (func (export "lunarwing:mcp/server@0.1.0#handle")
      (param $request i32) (param $request-len i32) (result i32)
      (local $call i32) (local $ptr i32) (local $len i32)
      global.get $calls
      local.tee $call
      i32.const 1
      i32.add
      global.set $calls

      local.get $call
      i32.const 0
      i32.eq
      if
        local.get $request
        local.get $request-len
        i32.const 5000
        i32.const __INITIALIZE_LEN__
        call $assert-request
        i32.const 1024
        local.set $ptr
        i32.const 140
        local.set $len
      else
        local.get $call
        i32.const 1
        i32.eq
        if
          local.get $request
          local.get $request-len
          i32.const 5200
          i32.const 54
          call $assert-request
          ;; notifications/initialized must produce no response.
          i32.const 0
          local.set $ptr
          i32.const 0
          local.set $len
        else
          local.get $call
          i32.const 2
          i32.eq
          if
            local.get $request
            local.get $request-len
            i32.const 5280
            i32.const 46
            call $assert-request
            i32.const 2048
            local.set $ptr
            i32.const 141
            local.set $len
          else
            local.get $call
            i32.const 3
            i32.eq
            if
              local.get $request
              local.get $request-len
              i32.const 5360
              i32.const 75
              call $assert-request
              i32.const 3072
              local.set $ptr
              i32.const 118
              local.set $len
            else
              local.get $call
              i32.const 4
              i32.eq
              if
                local.get $request
                local.get $request-len
                i32.const 5480
                i32.const 96
                call $assert-request
                i32.const 4096
                local.set $ptr
                i32.const 110
                local.set $len
              else
                ;; Sixth call selects one terminal resource behavior. Any
                ;; malformed envelope or unexpected sequence traps.
                local.get $request-len
                i32.const 49
                i32.eq
                if
                  local.get $request
                  local.get $request-len
                  i32.const 5620
                  i32.const 49
                  call $assert-request
                  unreachable
                else
                  local.get $request-len
                  i32.const 52
                  i32.eq
                  if
                    local.get $request
                    local.get $request-len
                    i32.const 5700
                    i32.const 52
                    call $assert-request
                    loop $spin
                      br $spin
                    end
                  else
                    local.get $request
                    local.get $request-len
                    i32.const 5790
                    i32.const 51
                    call $assert-request
                    i32.const 32
                    memory.grow
                    i32.const -1
                    i32.eq
                    if
                      unreachable
                    else
                      ;; Successful growth returns normally, making low-limit
                      ;; test fail because it expects limiter-induced trap.
                      i32.const 4600
                      local.set $ptr
                      i32.const 37
                      local.set $len
                    end
                  end
                end
              end
            end
          end
        end
      end

      i32.const 7168
      i32.const 0
      i32.store8
      i32.const 7168
      local.get $ptr
      i32.store offset=4
      i32.const 7168
      local.get $len
      i32.store offset=8
      i32.const 7168)
  )

  (core instance $server-instance (instantiate $server))
  (alias core export $server-instance "memory" (core memory $memory))
  (alias core export $server-instance "cabi_realloc" (core func $realloc))
  (alias core export $server-instance "lunarwing:mcp/server@0.1.0#handle" (core func $handle-core))

  (type $handle-type (func (param "request-json" string) (result (result string (error string)))))
  (func $handle (type $handle-type)
    (canon lift (core func $handle-core) (memory $memory) (realloc $realloc) string-encoding=utf8))
  (instance $server-exports
    (export "handle" (func $handle)))
  (export "lunarwing:mcp/server@0.1.0" (instance $server-exports))
)
