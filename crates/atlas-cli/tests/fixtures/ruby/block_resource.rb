def read_file
  File.open("data.txt") do |f|
    content = f.read
    puts content
  end
end
