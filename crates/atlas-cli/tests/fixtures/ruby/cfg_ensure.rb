def process(flag, fail_now)
  begin
    if flag
      return 1
    end
    if fail_now
      raise Error
    end
    work()
  rescue Error
    recover()
  else
    success()
  ensure
    cleanup()
  end
  after()
end
