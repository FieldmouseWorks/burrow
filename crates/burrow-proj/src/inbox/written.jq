def history: {totalCount:0,nodes:[]};
def text: {author:(.user + {databaseId:.user.id,__typename:.user.type}),body,userContentEdits:(.userContentEdits // history)};
{data:{repository:{issue:($i[0] | if type == "array" then map(select(.number == $n)) | first else . end | text + {title,timelineItems:(.timelineItems // history)}) +
  {comments:($c | flatten | map(text + {databaseId:.id,createdAt:.created_at}) | {totalCount:length,nodes:.})}}}}
