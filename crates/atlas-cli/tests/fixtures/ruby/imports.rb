require 'json'
require_relative 'helper'

include Enumerable

puts JSON.generate({ key: 'value' })
